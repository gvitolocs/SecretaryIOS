// Differential-test driver: run the retired Node Lucina's parser over the
// fixtures and print what it said, so tests/differential.rs can compare the
// Rust port with it.
import fs from 'node:fs';
import { parseComputerResult } from './computer-use.mjs';

const files = process.argv.slice(2);
const out = files.map((file) => {
  const output = fs.readFileSync(file, 'utf8');
  try {
    return { file, ok: true, value: parseComputerResult(output) };
  } catch (error) {
    const message = String((error && error.message) || error);
    let errorKind = 'no_completion';
    if (/invalid computer-use event stream/.test(message)) errorKind = 'invalid_stream';
    else if (/quota is exhausted/.test(message)) errorKind = 'quota_exhausted';
    return { file, ok: false, errorKind, message };
  }
});
process.stdout.write(JSON.stringify(out));
