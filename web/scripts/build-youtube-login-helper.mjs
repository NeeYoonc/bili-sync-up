#!/usr/bin/env node
// 把 web/static/youtube-login-extension/ 打包成 web/static/youtube-login-helper.zip。
//
// 扩展是随前端一起发布的静态文件，用户下载的是这个 zip。以前 zip 只在手工发布时
// 生成，源码改了 zip 不跟着变，用户拿到的还是几个月前的登录助手（例如抖音游客
// Cookie 校验这种修复根本到不了用户手里）。改动扩展源码后请执行：
//
//   cd web && npm run build:youtube-helper
//
// 这里不引入第三方依赖，直接写「仅存储」的 zip（扩展都是小文本文件，不值得压缩）。
import { readFileSync, writeFileSync, readdirSync, statSync } from 'node:fs';
import { join, dirname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const sourceDir = join(here, '..', 'static', 'youtube-login-extension');
const targetZip = join(here, '..', 'static', 'youtube-login-helper.zip');

const CRC_TABLE = (() => {
	const table = new Uint32Array(256);
	for (let index = 0; index < 256; index += 1) {
		let value = index;
		for (let bit = 0; bit < 8; bit += 1) {
			value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
		}
		table[index] = value >>> 0;
	}
	return table;
})();

function crc32(buffer) {
	let crc = 0xffffffff;
	for (const byte of buffer) {
		crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
	}
	return (crc ^ 0xffffffff) >>> 0;
}

function dosDateTime(date) {
	const time =
		((date.getHours() & 0x1f) << 11) |
		((date.getMinutes() & 0x3f) << 5) |
		(Math.floor(date.getSeconds() / 2) & 0x1f);
	const day =
		(((date.getFullYear() - 1980) & 0x7f) << 9) |
		(((date.getMonth() + 1) & 0x0f) << 5) |
		(date.getDate() & 0x1f);
	return { time, day };
}

const files = readdirSync(sourceDir)
	.filter((name) => statSync(join(sourceDir, name)).isFile())
	.sort();

if (files.length === 0) {
	throw new Error(`目录为空：${sourceDir}`);
}

const localParts = [];
const centralParts = [];
let offset = 0;

for (const name of files) {
	const data = readFileSync(join(sourceDir, name));
	const nameBytes = Buffer.from(name, 'utf8');
	const stat = statSync(join(sourceDir, name));
	const { time, day } = dosDateTime(stat.mtime);
	const checksum = crc32(data);

	const local = Buffer.alloc(30);
	local.writeUInt32LE(0x04034b50, 0);
	local.writeUInt16LE(20, 4); // version needed
	local.writeUInt16LE(0x0800, 6); // UTF-8 文件名
	local.writeUInt16LE(0, 8); // 仅存储
	local.writeUInt16LE(time, 10);
	local.writeUInt16LE(day, 12);
	local.writeUInt32LE(checksum, 14);
	local.writeUInt32LE(data.length, 18);
	local.writeUInt32LE(data.length, 22);
	local.writeUInt16LE(nameBytes.length, 26);
	local.writeUInt16LE(0, 28);
	localParts.push(local, nameBytes, data);

	const central = Buffer.alloc(46);
	central.writeUInt32LE(0x02014b50, 0);
	central.writeUInt16LE(20, 4); // version made by
	central.writeUInt16LE(20, 6); // version needed
	central.writeUInt16LE(0x0800, 8);
	central.writeUInt16LE(0, 10);
	central.writeUInt16LE(time, 12);
	central.writeUInt16LE(day, 14);
	central.writeUInt32LE(checksum, 16);
	central.writeUInt32LE(data.length, 20);
	central.writeUInt32LE(data.length, 24);
	central.writeUInt16LE(nameBytes.length, 28);
	central.writeUInt16LE(0, 30); // extra
	central.writeUInt16LE(0, 32); // comment
	central.writeUInt16LE(0, 34); // disk
	central.writeUInt16LE(0, 36); // internal attrs
	central.writeUInt32LE(0, 38); // external attrs
	central.writeUInt32LE(offset, 42);
	centralParts.push(central, nameBytes);

	offset += local.length + nameBytes.length + data.length;
}

const centralSize = centralParts.reduce((total, part) => total + part.length, 0);
const end = Buffer.alloc(22);
end.writeUInt32LE(0x06054b50, 0);
end.writeUInt16LE(0, 4);
end.writeUInt16LE(0, 6);
end.writeUInt16LE(files.length, 8);
end.writeUInt16LE(files.length, 10);
end.writeUInt32LE(centralSize, 12);
end.writeUInt32LE(offset, 16);
end.writeUInt16LE(0, 20);

writeFileSync(targetZip, Buffer.concat([...localParts, ...centralParts, end]));
console.log(`已生成 ${basename(targetZip)}：${files.length} 个文件（${files.join(', ')}）`);
