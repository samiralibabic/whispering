// Verifies Tauri updater signatures (minisign format) against the public key in tauri.conf.json.
// Usage: bun apps/whispering/scripts/release/verify-signatures.ts <tauri.conf.json> <file> [file...]   (each file needs file.sig)
import { createHash, createPublicKey, verify } from 'node:crypto';
import { readFileSync } from 'node:fs';

const [confPath, ...files] = process.argv.slice(2);
const conf = JSON.parse(readFileSync(confPath, 'utf8'));
const pubText = Buffer.from(conf.plugins.updater.pubkey, 'base64').toString('utf8');
const pubBytes = Buffer.from(pubText.trim().split('\n')[1], 'base64'); // "Ed" + keyId(8) + key(32)
const keyId = pubBytes.subarray(2, 10);
const key = createPublicKey({
	key: { kty: 'OKP', crv: 'Ed25519', x: pubBytes.subarray(10).toString('base64url') },
	format: 'jwk',
});

let failed = false;
for (const file of files) {
	const sigText = Buffer.from(readFileSync(`${file}.sig`, 'utf8'), 'base64').toString('utf8');
	const lines = sigText.trim().split('\n');
	const sig = Buffer.from(lines[1], 'base64'); // alg(2) + keyId(8) + signature(64)
	const trusted = lines[2].replace(/^trusted comment: /, '');
	const globalSig = Buffer.from(lines[3], 'base64');
	const alg = sig.subarray(0, 2).toString('latin1');
	const data = readFileSync(file);
	const message = alg === 'ED' ? createHash('blake2b512').update(data).digest() : data;
	const keyMatches = sig.subarray(2, 10).equals(keyId);
	const fileOk = verify(null, message, key, sig.subarray(10));
	const globalOk = verify(null, Buffer.concat([sig.subarray(10), Buffer.from(trusted)]), key, globalSig);
	const ok = keyMatches && fileOk && globalOk;
	failed ||= !ok;
	console.log(`${ok ? 'OK  ' : 'FAIL'} ${file.split('/').pop()} (key ${keyMatches}, file ${fileOk}, comment ${globalOk})`);
}
process.exit(failed ? 1 : 0);
