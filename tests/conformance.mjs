// Execute the pinned upstream comparison and support rules directly (Node 24 type stripping).
import { pathToFileURL } from 'node:url';
import { readFileSync, writeFileSync, readdirSync, mkdirSync, copyFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { isDeepStrictEqual } from 'node:util';

const [root, dll, output] = process.argv.slice(2).map(p => resolve(p));
const runners = join(root, 'tests/conformance/scripts/run-tests/runners');
const { StreamedReadTestRunner } = await import(pathToFileURL(join(runners, 'TestRunner.ts')));
const { default: Indexed } = await import(pathToFileURL(join(runners, 'RustIndexedReaderTestRunner.ts')));
const { default: Writer } = await import(pathToFileURL(join(runners, 'RustWriterTestRunner.ts')));
const results = [];
mkdirSync(output, { recursive: true });
const cases = readdirSync(join(root, 'tests/conformance/data'), { recursive: true }).filter(p => p.endsWith('.json')).sort();
if (cases.length !== 416) throw new Error(`Expected 416 pinned cases, got ${cases.length}`);
function run(command, input, destination) {
  const args = [dll, command, input];
  if (destination) args.push(destination);
  const r = spawnSync('dotnet', args, { timeout: 30000, maxBuffer: 16 * 1024 * 1024, encoding: 'utf8' });
  if (r.error || r.status !== 0) throw new Error(`${command}: ${r.error ?? r.stderr}`);
  return r.stdout;
}
try {
  for (const name of cases) {
    const spec = join(root, 'tests/conformance/data', name);
    const input = spec.replace(/\.json$/, '.mcap');
    const test = JSON.parse(readFileSync(spec, 'utf8'));
    const variant = { records: test.records, features: new Set(test.meta.variant.features) };
    for (const [mode, runner] of [['stream', new StreamedReadTestRunner()], ['indexed', new Indexed()], ['write', new Writer()]]) {
      const supported = mode === 'stream' || runner.supportsVariant(variant);
      const result = { name, mode, status: supported ? 'running' : 'unsupported' };
      results.push(result);
      if (!supported) {
        result.reason = mode === 'write' ? 'Official Rust writer cannot emit pad variants' : 'Official Rust indexed runner requires messages and ch/chx/rch/rsh';
        continue;
      }
      try {
        if (mode === 'write') {
          const dest = join(output, name.replaceAll(/[\\/]/g, '_') + '.mcap');
          run(mode, spec, dest);
          if (!readFileSync(dest).equals(readFileSync(input))) throw new Error('Byte-for-byte writer mismatch');
        } else {
          const actual = JSON.parse(run(mode, input));
          const expected = runner.expectedResult(test);
          if (!isDeepStrictEqual(actual, expected)) {
            writeFileSync(join(output, 'actual.json'), JSON.stringify(actual, null, 2));
            writeFileSync(join(output, 'expected.json'), JSON.stringify(expected, null, 2));
            throw new Error('Official expectedResult comparison failed');
          }
        }
        result.status = 'passed';
      } catch (e) {
        result.status = 'failed'; result.error = String(e);
        copyFileSync(spec, join(output, 'failed-spec.json'));
        copyFileSync(input, join(output, 'failed-input.mcap'));
        throw new Error(`${name}/${mode}: ${e}`);
      }
    }
  }
} finally {
  writeFileSync(join(output, 'conformance.json'), JSON.stringify(results, null, 2));
}
for (const [mode, count] of [['stream', 416], ['indexed', 32], ['write', 208]]) {
  if (results.filter(r => r.mode === mode && r.status === 'passed').length !== count) throw new Error(`Support inventory changed: ${mode}`);
}
console.log(JSON.stringify(Object.groupBy(results, r => `${r.mode}/${r.status}`), (key, value) => Array.isArray(value) ? value.length : value));
