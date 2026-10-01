//! Allocation-free bootstrap selection before a JSON document has a budget.
use super::*;
pub(super) struct Cursor<'a> { pub input: &'a [u8], pub position: usize }
impl Cursor<'_> {
    fn whitespace(&mut self) {
        while self.input.get(self.position).is_some_and(|b| matches!(b,b' '|b'\n'|b'\r'|b'\t')) {self.position+=1;}
    }
    fn eat(&mut self, byte:u8)->Outcome<()> {
        self.whitespace();
        if self.input.get(self.position)!=Some(&byte) {return Err("Invalid JSON delimiter".into());}
        self.position+=1; Ok(())
    }
    fn hex(&mut self) -> Outcome<u32> {
        let bytes = self
            .input
            .get(self.position..self.position + 4)
            .ok_or("Truncated JSON escape")?;
        let mut result = 0;
        for b in bytes {
            result = result * 16
                + match b {
                    b'0'..=b'9' => (b - b'0') as u32,
                    b'a'..=b'f' => (b - b'a' + 10) as u32,
                    b'A'..=b'F' => (b - b'A' + 10) as u32,
                    _ => return Err("Invalid JSON unicode escape".into()),
                };
        }
        self.position += 4;
        Ok(result)
    }
    pub(super) fn string(&mut self, mut emit: impl FnMut(&[u8])) -> Outcome<()> {
        self.eat(b'"')?;
        loop {
            let begin = self.position;
            while self
                .input
                .get(self.position)
                .is_some_and(|b| *b >= 0x20 && *b != b'"' && *b != b'\\')
            {
                self.position += 1;
            }
            let run = &self.input[begin..self.position];
            std::str::from_utf8(run)?;
            emit(run);
            match self.input.get(self.position) {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.position += 1;
                    let escape = *self
                        .input
                        .get(self.position)
                        .ok_or("Truncated JSON escape")?;
                    self.position += 1;
                    let c = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut code = self.hex()?;
                            if (0xd800..=0xdbff).contains(&code) {
                                if self.input.get(self.position..self.position + 2) != Some(b"\\u")
                                {
                                    return Err("Missing JSON low surrogate".into());
                                }
                                self.position += 2;
                                let low = self.hex()?;
                                if !(0xdc00..=0xdfff).contains(&low) {
                                    return Err("Invalid JSON low surrogate".into());
                                }
                                code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                            }
                            char::from_u32(code).ok_or("Invalid JSON unicode scalar")?
                        }
                        _ => return Err("Invalid JSON escape".into()),
                    };
                    emit(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                _ => return Err("Unterminated JSON string or control byte".into()),
            }
        }
    }

    fn value(&mut self, path: Option<&[&str]>, depth:usize, presence:bool)->Outcome<Option<u64>> {
        self.whitespace();
        let byte=*self.input.get(self.position).ok_or("Missing JSON value")?;
        let selected: Outcome<Option<u64>> = match byte {
            b'"'=>{self.string(|_|{})?;Ok(None)}
            b'{'|b'['=>{
                if depth>=127 {return Err("JSON recursion limit exceeded".into());}
                self.position+=1; self.whitespace();
                let end=if byte==b'{' {b'}'} else {b']'};
                let mut selected=None;
                if self.input.get(self.position)!=Some(&end) {
                    loop {
                        let mut matched=false;
                        if byte==b'{' {
                            let key=path.and_then(|p|p.first()).map(|s|s.as_bytes());
                            let mut offset=0usize;
                            let mut equal=key.is_some();
                            self.string(|run| {
                                if let Some(key)=key {equal &= key.get(offset..offset+run.len())==Some(run);}
                                offset+=run.len();
                            })?;
                            matched=equal && key.is_some_and(|k|k.len()==offset);
                            self.eat(b':')?;
                        }
                        let child=if matched {path.map(|p|&p[1..])} else {None};
                        let value=self.value(child,depth+1,presence)?;
                        if matched {selected=value;}
                        self.whitespace();
                        if self.input.get(self.position)==Some(&end) {break;}
                        self.eat(b',')?;
                    }
                }
                self.eat(end)?; Ok(selected)
            }
            b'n'|b't'|b'f'=>{
                let token:&[u8]=match byte {b'n'=>b"null",b't'=>b"true",_=>b"false"};
                if self.input.get(self.position..self.position+token.len())!=Some(token) {return Err("Invalid JSON literal".into());}
                self.position+=token.len(); Ok(None)
            }
            b'-'|b'0'..=b'9'=>{
                let start=self.position;
                if byte==b'-' {self.position+=1;}
                match self.input.get(self.position) {
                    Some(b'0')=>self.position+=1,
                    Some(b'1'..=b'9')=>self.digits(),
                    _=>return Err("Invalid JSON number".into()),
                }
                let mut integer=byte!=b'-';
                if self.input.get(self.position)==Some(&b'.') {
                    integer=false; self.position+=1;
                    let begin=self.position;self.digits();
                    if begin==self.position {return Err("Invalid JSON fraction".into());}
                }
                if self.input.get(self.position).is_some_and(|b|matches!(b,b'e'|b'E')) {
                    integer=false;self.position+=1;
                    if self.input.get(self.position).is_some_and(|b|matches!(b,b'+'|b'-')) {self.position+=1;}
                    let begin=self.position;self.digits();
                    if begin==self.position {return Err("Invalid JSON exponent".into());}
                }
                if integer && path==Some(&[]) {
                    Ok(std::str::from_utf8(&self.input[start..self.position])?.parse().ok())
                } else {Ok(None)}
            }
            _=>Err("Invalid JSON value".into()),
        };
        let selected=selected?;
        if presence && path==Some(&[]) { Ok((byte!=b'n').then_some(1)) } else { Ok(selected) }
    }
    fn digits(&mut self) {while self.input.get(self.position).is_some_and(u8::is_ascii_digit) {self.position+=1;}}
}
pub(crate) fn select_id(input:&[u8],path:&[&str])->Outcome<u64> {
    let mut cursor=Cursor {input,position:0};
    let id=cursor.value(Some(path),0,false)?.unwrap_or(0);
    cursor.whitespace();
    if cursor.position!=input.len() {return Err("Trailing JSON data".into());}
    Ok(id)
}

pub(crate) fn has_non_null(input:&[u8],path:&[&str])->Outcome<bool> {
    let mut cursor=Cursor {input,position:0};
    let found=cursor.value(Some(path),0,true)?.is_some();
    cursor.whitespace();
    if cursor.position!=input.len() {return Err("Trailing JSON data".into());}
    Ok(found)
}
