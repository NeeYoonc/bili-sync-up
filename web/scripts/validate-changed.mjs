import { execSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import process from 'node:process';

const webRoot = process.cwd();
const repoRoot = path.resolve(webRoot, '..');

function run(command, options = {}) {
	console.log(`\n$ ${command}`);
	execSync(command, {
		cwd: options.cwd ?? webRoot,
		stdio: 'inherit',
		env: process.env,
		shell: true
	});
}

function collectChangedFiles() {
	const output = execSync('git status --porcelain', {
		cwd: repoRoot,
		encoding: 'utf8'
	});

	return output
		.split(/\r?\n/)
		.filter(Boolean)
		.map((line) => line.slice(3).trim())
		.filter((file) => file.startsWith('web/'))
		.map((file) => file.replace(/^web\//, ''))
		.filter((file) => existsSync(path.join(webRoot, file)));
}

function shellQuote(file) {
	return JSON.stringify(file);
}

const changedFiles = collectChangedFiles();
if (changedFiles.length === 0) {
	console.log('No changed files under web/. Skipping changed-file validation.');
	process.exit(0);
}

console.log('Changed files under web/:');
for (const file of changedFiles) {
	console.log(`- ${file}`);
}

const prettierTargets = changedFiles.filter((file) =>
	/\.(svelte|ts|js|mjs|json|md|css)$/i.test(file)
);
const eslintTargets = changedFiles.filter((file) => /\.(svelte|ts|js|mjs)$/i.test(file));

if (prettierTargets.length > 0) {
	run(
		`node ./node_modules/prettier/bin/prettier.cjs --check ${prettierTargets.map(shellQuote).join(' ')}`,
		{ cwd: webRoot }
	);
}

if (eslintTargets.length > 0) {
	run(`node ./node_modules/eslint/bin/eslint.js ${eslintTargets.map(shellQuote).join(' ')}`, {
		cwd: webRoot
	});
}

console.log('\nChanged-file validation passed.');
