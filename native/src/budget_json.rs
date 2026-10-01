//! Charged JSON control storage: one string arena and paged nodes, no Value tree.
//! Object members use an AVL index; arrays retain their original order.
use super::*;
#[path = "budget_json_scan.rs"]
mod scan;
#[cfg(test)]
pub(super) use scan::select_id;
use mcap::{
    segmented::BudgetedSegmentedVec,
    storage::{OwnerKind, Reservation, ResourceCategory},
};
use std::ops::Range;
#[derive(Clone)]
enum Kind {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(Range<usize>),
    Array(Option<usize>),
    Object(Option<usize>),
}
struct Node {
    kind: Kind,
    key: Range<usize>,
    left: Option<usize>,
    right: Option<usize>,
    next: Option<usize>,
    height: u8,
}
pub(super) struct Document {
    text: Vec<u8>,
    charge: Reservation,
    nodes: BudgetedSegmentedVec<Node>,
    root: usize,
}
impl Document {
    pub fn configured(input:&[u8], path:&[&str]) -> Outcome<(Self,mcap::storage::BudgetRef)> {
        let domain=budget::resolve(scan::select_id(input,path)?)?;
        Ok((Self::parse(input,&domain)?,domain))
    }
    pub fn parse(input: &[u8], domain: &mcap::storage::BudgetRef) -> Outcome<Self> {
        memory::check(
            domain, "StorageBlock",
            Some(domain.limits().block as u64),
            input.len(),
        )?;
        let (text, charge) =
            mcap::charged::vector_fixed(domain, ResourceCategory::Declaration, input.len())?;
        charge.owner_reference(OwnerKind::Operation, true);
        let mut doc = Self {
            text,
            charge,
            nodes: BudgetedSegmentedVec::new(domain.clone(), ResourceCategory::Declaration),
            root: 0,
        };
        let mut parser = Parser {
            input,
            position: 0,
            doc: &mut doc,
        };
        let root = parser.value(0)?;
        parser.whitespace();
        if parser.position != input.len() {
            return Err("Trailing JSON data".into());
        }
        doc.root = root;
        doc.nodes.owner_reference(OwnerKind::Operation, true);
        Ok(doc)
    }
    pub fn view(&self) -> View<'_> {
        View {
            doc: self,
            index: Some(self.root),
        }
    }
    fn text(&self, r: &Range<usize>) -> &str {
        std::str::from_utf8(&self.text[r.clone()]).expect("validated JSON string")
    }
    fn height(&self, n: Option<usize>) -> u8 {
        n.map_or(0, |n| self.nodes[n].height)
    }
    fn update(&mut self, n: usize) {
        self.nodes[n].height = 1 + self
            .height(self.nodes[n].left)
            .max(self.height(self.nodes[n].right));
    }
    fn rotate_left(&mut self, n: usize) -> usize {
        let r = self.nodes[n].right.unwrap();
        self.nodes[n].right = self.nodes[r].left;
        self.nodes[r].left = Some(n);
        self.update(n);
        self.update(r);
        r
    }
    fn rotate_right(&mut self, n: usize) -> usize {
        let l = self.nodes[n].left.unwrap();
        self.nodes[n].left = self.nodes[l].right;
        self.nodes[l].right = Some(n);
        self.update(n);
        self.update(l);
        l
    }
    fn insert(&mut self, root: Option<usize>, node: usize) -> usize {
        let Some(r) = root else { return node };
        match self
            .text(&self.nodes[node].key)
            .cmp(self.text(&self.nodes[r].key))
        {
            std::cmp::Ordering::Less => {
                let child = self.insert(self.nodes[r].left, node);
                self.nodes[r].left = Some(child);
            }
            std::cmp::Ordering::Greater => {
                let child = self.insert(self.nodes[r].right, node);
                self.nodes[r].right = Some(child);
            }
            std::cmp::Ordering::Equal => {
                self.nodes[r].kind = self.nodes[node].kind.clone();
                return r;
            }
        }
        self.update(r);
        let balance =
            self.height(self.nodes[r].left) as i16 - self.height(self.nodes[r].right) as i16;
        if balance > 1 {
            let l = self.nodes[r].left.unwrap();
            if self.height(self.nodes[l].left) < self.height(self.nodes[l].right) {
                let l = self.rotate_left(l);
                self.nodes[r].left = Some(l);
            }
            return self.rotate_right(r);
        }
        if balance < -1 {
            let right = self.nodes[r].right.unwrap();
            if self.height(self.nodes[right].right) < self.height(self.nodes[right].left) {
                let right = self.rotate_right(right);
                self.nodes[r].right = Some(right);
            }
            return self.rotate_left(r);
        }
        r
    }
}
#[derive(Clone, Copy)]
pub(super) struct View<'a> {
    doc: &'a Document,
    index: Option<usize>,
}
impl<'a> View<'a> {
    fn kind(self) -> Option<&'a Kind> {
        self.index.map(|i| &self.doc.nodes[i].kind)
    }
    pub fn get(self, key: &str) -> Self {
        let mut current = match self.kind() {
            Some(Kind::Object(root)) => *root,
            _ => None,
        };
        while let Some(i) = current {
            let node = &self.doc.nodes[i];
            match key.cmp(self.doc.text(&node.key)) {
                std::cmp::Ordering::Less => current = node.left,
                std::cmp::Ordering::Greater => current = node.right,
                std::cmp::Ordering::Equal => {
                    return Self {
                        doc: self.doc,
                        index: Some(i),
                    }
                }
            }
        }
        Self {
            doc: self.doc,
            index: None,
        }
    }
    pub fn is_null(self) -> bool {matches!(self.kind(),None|Some(Kind::Null))}
    pub fn as_str(self) -> Option<&'a str> {
        match self.kind() {
            Some(Kind::String(r)) => Some(self.doc.text(r)),
            _ => None,
        }
    }
    pub fn as_u64(self) -> Option<u64> {
        match self.kind() {
            Some(Kind::Number(n)) => n.as_u64(),
            _ => None,
        }
    }
    pub fn as_bool(self) -> Option<bool> {
        match self.kind() {
            Some(Kind::Bool(v)) => Some(*v),
            _ => None,
        }
    }
    fn members(self, mut f: impl FnMut(&'a str, Self) -> Outcome<()>) -> Outcome<()> {
        fn walk<'a>(
            view: View<'a>,
            root: Option<usize>,
            f: &mut impl FnMut(&'a str, View<'a>) -> Outcome<()>,
        ) -> Outcome<()> {
            if let Some(i) = root {
                let n = &view.doc.nodes[i];
                walk(view, n.left, f)?;
                f(
                    view.doc.text(&n.key),
                    View {
                        doc: view.doc,
                        index: Some(i),
                    },
                )?;
                walk(view, n.right, f)?;
            }
            Ok(())
        }
        let Some(Kind::Object(root)) = self.kind() else {
            return Err("Expected JSON object".into());
        };
        walk(self, *root, &mut f)
    }
}
struct Parser<'a, 'b> {
    input: &'a [u8],
    position: usize,
    doc: &'b mut Document,
}
impl Parser<'_, '_> {
    fn whitespace(&mut self) {
        while self
            .input
            .get(self.position)
            .is_some_and(|v| matches!(v, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }
    fn eat(&mut self, b: u8) -> Outcome<()> {
        self.whitespace();
        if self.input.get(self.position) != Some(&b) {
            return Err("Invalid JSON delimiter".into());
        }
        self.position += 1;
        Ok(())
    }
    fn string(&mut self) -> Outcome<Range<usize>> {
        let start=self.doc.text.len();
        let mut cursor=scan::Cursor {input:self.input,position:self.position};
        let result=cursor.string(|run| {
            self.doc.text.extend_from_slice(run);
            self.doc.charge.domain().copy_bytes(mcap::storage::CopyKind::Other,run.len());
        });
        self.position=cursor.position;
        result?;
        Ok(start..self.doc.text.len())
    }
    fn value(&mut self, depth: usize) -> Outcome<usize> {
        self.whitespace();
        let b = *self.input.get(self.position).ok_or("Missing JSON value")?;
        let kind =
            match b {
                b'"' => Kind::String(self.string()?),
                b'{' | b'[' => {
                    if depth >= 127 {
                        return Err("JSON recursion limit exceeded".into());
                    }
                    self.position += 1;
                    self.whitespace();
                    let end = if b == b'{' { b'}' } else { b']' };
                    let mut root = None;
                    let mut last = None;
                    if self.input.get(self.position) != Some(&end) {
                        loop {
                            let key = if b == b'{' {
                                let key = self.string()?;
                                self.eat(b':')?;
                                key
                            } else {
                                0..0
                            };
                            let item = self.value(depth + 1)?;
                            if b == b'{' {
                                self.doc.nodes[item].key = key;
                                root = Some(self.doc.insert(root, item));
                            } else {
                                if let Some(previous) = last {
                                    self.doc.nodes[previous].next = Some(item);
                                } else {
                                    root = Some(item);
                                }
                                last = Some(item);
                            }
                            self.whitespace();
                            if self.input.get(self.position) == Some(&end) {
                                break;
                            }
                            self.eat(b',')?;
                        }
                    }
                    self.eat(end)?;
                    if b == b'{' {
                        Kind::Object(root)
                    } else {
                        Kind::Array(root)
                    }
                }
                b'n' | b't' | b'f' => {
                    let (token, kind): (&[u8], Kind) = match b {
                        b'n' => (b"null", Kind::Null),
                        b't' => (b"true", Kind::Bool(true)),
                        _ => (b"false", Kind::Bool(false)),
                    };
                    if self.input.get(self.position..self.position + token.len()) != Some(token) {
                        return Err("Invalid JSON literal".into());
                    }
                    self.position += token.len();
                    kind
                }
                b'-' | b'0'..=b'9' => {
                    let start = self.position;
                    while self.input.get(self.position).is_some_and(|b| {
                        matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                    }) {
                        self.position += 1;
                    }
                    Kind::Number(serde_json::from_slice(&self.input[start..self.position])?)
                }
                _ => return Err("Invalid JSON value".into()),
            };
        let index = self.doc.nodes.len();
        self.doc.nodes.push_fixed(Node {
            kind,
            key: 0..0,
            left: None,
            right: None,
            next: None,
            height: 1,
        })?;
        Ok(index)
    }
}
#[derive(Clone, Copy)]
pub(super) enum Control<'a> {
    Existing(&'a Value),
    Charged(View<'a>),
}

// AVL height is below twice the address width for any addressable node set.
// This traversal stack lives on the call stack and never allocates a directory.
#[derive(Clone)]
pub(super) enum StringPairs<'a> {
    Existing(serde_json::map::Iter<'a>),
    Charged {
        view: View<'a>,
        stack: [usize; 2 * usize::BITS as usize],
        depth: usize,
        current: Option<usize>,
    },
}
impl<'a> Iterator for StringPairs<'a> {
    type Item = (&'a str, &'a str);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Existing(iter) => iter.next().map(|(k,v)| (k.as_str(),v.as_str().expect("validated metadata"))),
            Self::Charged { view,stack,depth,current } => {
                while let Some(i)=*current {
                    stack[*depth]=i; *depth+=1; *current=view.doc.nodes[i].left;
                }
                if *depth==0 { return None; }
                *depth-=1;
                let i=stack[*depth];
                let node=&view.doc.nodes[i];
                *current=node.right;
                Some((view.doc.text(&node.key),View {doc:view.doc,index:Some(i)}.as_str().expect("validated metadata")))
            }
        }
    }
}
impl<'a> Control<'a> {
    pub fn contains_key(self,key:&str)->bool {
        match self {Self::Existing(v)=>v.get(key).is_some(),Self::Charged(v)=>v.get(key).index.is_some()}
    }

    /// Validate before writer advancement, then iterate immutable fields directly.
    pub fn strings(self) -> Outcome<StringPairs<'a>> {
        match self {
            Self::Existing(v) => {
                let map=v.as_object().ok_or("Metadata must be an object")?;
                if map.values().any(|v| !v.is_string()) { return Err("Metadata values must be strings".into()); }
                Ok(StringPairs::Existing(map.iter()))
            }
            Self::Charged(view) => {
                view.members(|_,v| {v.as_str().ok_or("Metadata values must be strings")?;Ok(())})?;
                let Some(Kind::Object(root))=view.kind() else {unreachable!()};
                Ok(StringPairs::Charged {view,stack:[0;2*usize::BITS as usize],depth:0,current:*root})
            }
        }
    }
    pub fn get(self, key: &str) -> Self {
        match self {
            Self::Existing(v) => Self::Existing(&v[key]),
            Self::Charged(v) => Self::Charged(v.get(key)),
        }
    }
    pub fn as_str(self) -> Option<&'a str> {
        match self {
            Self::Existing(v) => v.as_str(),
            Self::Charged(v) => v.as_str(),
        }
    }
    pub fn as_u64(self) -> Option<u64> {
        match self {
            Self::Existing(v) => v.as_u64(),
            Self::Charged(v) => v.as_u64(),
        }
    }
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Existing(v) => v.as_bool(),
            Self::Charged(v) => v.as_bool(),
        }
    }
    pub fn string(self, key: &str) -> Outcome<&'a str> {
        self.get(key)
            .as_str()
            .ok_or_else(|| Error::message(format_args!("Missing string: {key}")))
    }
    pub fn number(self, key: &str) -> Outcome<u64> {
        self.get(key)
            .as_u64()
            .ok_or_else(|| Error::message(format_args!("Missing integer: {key}")))
    }
    // Official writer metadata still requires a BTreeMap. Its temporary and
    // retained ownership are migrated separately from this control descriptor.
    pub fn is_null(self) -> bool {
        match self { Self::Existing(v) => v.is_null(), Self::Charged(v) => matches!(v.kind(), None | Some(Kind::Null)) }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    fn owned(view: View<'_>) -> Value {
        match view.kind().unwrap() {
            Kind::Null => Value::Null,
            Kind::Bool(v) => Value::Bool(*v),
            Kind::Number(n) => Value::Number(n.clone()),
            Kind::String(r) => Value::String(view.doc.text(r).into()),
            Kind::Array(root) => {
                let mut out = Vec::new();
                let mut current = *root;
                while let Some(i) = current {
                    out.push(owned(View {
                        doc: view.doc,
                        index: Some(i),
                    }));
                    current = view.doc.nodes[i].next;
                }
                Value::Array(out)
            }
            Kind::Object(_) => {
                let mut out = serde_json::Map::new();
                view.members(|key, v| {
                    out.insert(key.into(), owned(v));
                    Ok(())
                })
                .unwrap();
                Value::Object(out)
            }
        }
    }
    fn check(input: &[u8], domain: &mcap::storage::BudgetRef) {
        let standard = serde_json::from_slice::<Value>(input);
        let actual = Document::parse(input, domain);
        assert_eq!(
            actual.is_ok(),
            standard.is_ok(),
            "input={:?}",
            String::from_utf8_lossy(input)
        );
        if let (Ok(actual), Ok(standard)) = (actual, standard) {
            assert_eq!(owned(actual.view()), standard);
        }
        assert_eq!(domain.workload_statistics().current, 0);
        assert_eq!(domain.ownership_statistics(), Default::default());
    }
    #[test]
    fn matches_standard_json_for_unicode_numbers_duplicates_and_malformed_inputs() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let samples = [
            r#"{"a":1,"a":2,"nested":[null,true,false,{},[],{"x":"\uD83D\uDE80"}]}"#,
            r#"{"\u0061":"\u0000\b\f\n\r\t\\\/\"","a":"é/通道"}"#,
            r#"[-0,0,1,-1,1.5,1e3,18446744073709551615,18446744073709551616,-9223372036854775809]"#,
            r#"{"z":{},"b":[],"c":{"inner":3},"b":{"replacement":true}}"#,
        ];
        for sample in samples {
            check(sample.as_bytes(), &domain);
            for end in 0..sample.len() {
                check(&sample.as_bytes()[..end], &domain);
            }
            for i in 0..sample.len() {
                for byte in [0, b' ', b'"', b'\\', b',', b':', b']', b'}', 0xff] {
                    let mut changed = sample.as_bytes().to_vec();
                    changed[i] = byte;
                    check(&changed, &domain);
                }
            }
        }
        for sample in [
            r#""\uD800""#,
            r#""\uDC00""#,
            r#""\uD800\u0000""#,
            "1e9999",
            "01",
            "-",
            "[1,]",
            "{\"a\":1,}",
        ] {
            check(sample.as_bytes(), &domain);
        }
        for depth in 125..130 {
            check(
                format!("{}0{}", "[".repeat(depth), "]".repeat(depth)).as_bytes(),
                &domain,
            );
        }
    }
    #[test]
    fn paged_object_index_handles_ordered_keys_and_duplicate_replacement() {
        let domain = mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let mut input = String::from("{");
        for i in 0..10000 {
            if i != 0 {
                input.push(',');
            }
            input.push_str(&format!("\"key{i:05}\":{i}"));
        }
        input.push_str(",\"key00042\":99999}");
        let doc = Document::parse(input.as_bytes(), &domain).unwrap();
        for i in 0..10000 {
            assert_eq!(
                doc.view().get(&format!("key{i:05}")).as_u64(),
                Some(if i == 42 { 99999 } else { i })
            );
        }
        let Kind::Object(root) = doc.view().kind().unwrap() else {
            panic!()
        };
        assert!(doc.height(*root) < 20);
        let peak = domain.workload_statistics().peak;
        drop(doc);
        assert_eq!(domain.workload_statistics().current, 0);
        let limited = mcap::storage::BudgetRef::new(mcap::storage::BudgetLimits {
            total: (peak as usize - 1) + mcap::storage::BudgetRef::allocation_size(),
            block: peak as usize - 1,
            retained: 0,
        }).unwrap();
        assert!(Document::parse(input.as_bytes(), &limited).is_err());
        assert_eq!(limited.workload_statistics().current, 0);
        assert_eq!(limited.ownership_statistics(), Default::default());
    }
}

#[cfg(test)]
mod pair_tests {
    use super::*;
    #[test]
    fn pairs_are_sorted_repeatable_and_validate_all_values_before_use() {
        let domain=mcap::storage::BudgetRef::new(Default::default()).unwrap();
        let input=serde_json::to_vec(&json!({"b":"two","a":"one","empty":""})).unwrap();
        let doc=Document::parse(&input,&domain).unwrap();
        let mut pairs=Control::Charged(doc.view()).strings().unwrap();
        assert_eq!(pairs.next(),Some(("a","one")));
        assert_eq!(pairs.clone().collect::<Vec<_>>(),vec![("b","two"),("empty","")]);
        assert_eq!(pairs.collect::<Vec<_>>(),vec![("b","two"),("empty","")]);
        for input in [b"null".as_slice(),br#"{"a":"ok","z":5}"#] {
            let doc=Document::parse(input,&domain).unwrap();
            assert!(Control::Charged(doc.view()).strings().is_err());
        }
    }
}

#[cfg(test)]
mod selector_tests {
    use super::*;
    #[test]
    fn bootstrap_selection_matches_last_value_and_escaped_keys() {
        for sample in [
            r#"{}"#, r#"null"#, r#"[1,2,3]"#,
            r#"{"Budget":{"id":7}}"#,
            r#"{"Budget":{"id":7,"id":9}}"#,
            r#"{"Budget":{"id":7},"Budget":null}"#,
            r#"{"Budget":{"id":7},"Budget":{"other":9}}"#,
            r#"{"Budget":{"id":7},"Budget":{"id":11}}"#,
            r#"{"Bu\u0064get":{"\u0069d":18446744073709551615},"text":"\ud83d\ude00"}"#,
            r#"{"Budget":{"id":-0}}"#, r#"{"Budget":{"id":1.0}}"#,
            r#"{"Budget":{"id":1e1}}"#, r#"{"Budget":{"id":18446744073709551616}}"#,
            r#"{"Budget":{"id":"7"}}"#, r#"{"Budget":{"id":[7]}}"#,
        ] {
            let value:Value=serde_json::from_str(sample).unwrap();
            let expected=value.get("Budget").and_then(|v|v.get("id")).and_then(Value::as_u64).unwrap_or(0);
            assert_eq!(select_id(sample.as_bytes(),&["Budget","id"]).unwrap(),expected,"{sample}");
            for end in 0..sample.len() {
                if serde_json::from_slice::<Value>(&sample.as_bytes()[..end]).is_err() {
                    assert!(select_id(&sample.as_bytes()[..end],&["Budget","id"]).is_err(),"{sample} end={end}");
                }
            }
        }
        for invalid in [r#"{"Budget":{"id":01}}"#,r#"{"Budget":{"id":1.}}"#,r#"{"Budget":{"id":1e+}}"#,r#"{"\ud800":0}"#,r#"{"a":1,}"#,r#"[1,]"#] {
            assert!(select_id(invalid.as_bytes(),&["Budget","id"]).is_err());
        }
    }
}
