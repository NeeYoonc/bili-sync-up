import assert from 'node:assert/strict';
import {
	buildNetscapeCookies,
	isDouyinCookieDomain,
	isYouTubeCookieDomain,
	normalizeServerUrl,
	permissionPattern,
	responseMessage
} from '../static/youtube-login-extension/helper.js';

assert.equal(normalizeServerUrl('http://127.0.0.1:12345/settings'), 'http://127.0.0.1:12345');
assert.equal(
	permissionPattern('https://nas.example.com:9443/settings'),
	'https://nas.example.com/*'
);
assert.equal(isYouTubeCookieDomain('.youtube.com'), true);
assert.equal(isYouTubeCookieDomain('accounts.google.com'), true);
assert.equal(isDouyinCookieDomain('.douyin.com'), true);
assert.equal(isDouyinCookieDomain('.bytedance.com'), true);

const exported = buildNetscapeCookies([
	{
		domain: '.youtube.com',
		path: '/',
		secure: true,
		expirationDate: 2_000_000_000.9,
		name: '__Secure-3PSID',
		value: 'youtube-session'
	},
	{
		domain: '.accounts.google.com',
		path: '/',
		secure: true,
		expirationDate: 2_000_000_000,
		name: 'SID',
		value: 'google-session'
	}
]);
assert.match(exported, /^# Netscape HTTP Cookie File/m);
assert.match(exported, /# Bili Sync Login Helper 1\.5\.3/);
assert.match(
	exported,
	/\.youtube\.com\tTRUE\t\/\tTRUE\t2000000000\t__Secure-3PSID\tyoutube-session/
);
assert.match(exported, /accounts\.google\.com\tTRUE\t\/\tTRUE\t2000000000\tSID\tgoogle-session/);
assert.throws(() => buildNetscapeCookies([]), /没有检测到 YouTube 登录状态/);
// 游客 Cookie（只有 ttwid 这类设备 Cookie）必须被拒绝，否则会导入一个「未登录」的会话。
assert.throws(
	() =>
		buildNetscapeCookies(
			[
				{
					domain: '.douyin.com',
					path: '/',
					secure: true,
					expirationDate: 2_000_000_000,
					name: 'ttwid',
					value: 'douyin-guest'
				}
			],
			'douyin'
		),
	/没有检测到 抖音 登录状态/
);
// 带登录会话 Cookie 时正常导出。
assert.match(
	buildNetscapeCookies(
		[
			{
				domain: '.douyin.com',
				path: '/',
				secure: true,
				expirationDate: 2_000_000_000,
				name: 'ttwid',
				value: 'douyin-guest'
			},
			{
				domain: '.douyin.com',
				path: '/',
				secure: true,
				expirationDate: 2_000_000_000,
				name: 'sessionid',
				value: 'douyin-session'
			}
		],
		'douyin'
	),
	/\.douyin\.com\tTRUE\t\/\tTRUE\t2000000000\tsessionid\tdouyin-session/
);
assert.match(
	buildNetscapeCookies(
		[
			{
				domain: '.bytedance.com',
				path: '/',
				secure: true,
				expirationDate: 2_000_000_000,
				name: 'uid_tt',
				value: 'shared-douyin-session'
			}
		],
		'douyin'
	),
	/\.bytedance\.com\tTRUE\t\/\tTRUE\t2000000000\tuid_tt\tshared-douyin-session/
);
assert.equal(responseMessage({ data: { message: '已导入' } }, 'fallback'), '已导入');
assert.equal(responseMessage({ data: '验证失败' }, 'fallback'), '验证失败');

console.log('YouTube login helper tests passed');
