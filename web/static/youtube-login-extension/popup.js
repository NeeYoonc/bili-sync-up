import {
	buildNetscapeCookies,
	normalizeServerUrl,
	permissionPattern,
	responseMessage
} from './helper.js';

const serverInput = document.querySelector('#server-url');
const tokenInput = document.querySelector('#auth-token');
const connectButton = document.querySelector('#connect-page');
const openButton = document.querySelector('#open-youtube');
const transferButton = document.querySelector('#transfer');
const openDouyinButton = document.querySelector('#open-douyin');
const transferDouyinButton = document.querySelector('#transfer-douyin');
const openTikTokButton = document.querySelector('#open-tiktok');
const transferTikTokButton = document.querySelector('#transfer-tiktok');
const statusBox = document.querySelector('#status');

// 显示扩展版本号（从 manifest 读取，避免硬编码不同步）
(() => {
	const versionEl = document.querySelector('#ext-version');
	try {
		const manifest = chrome.runtime.getManifest();
		if (versionEl && manifest && manifest.version) {
			versionEl.textContent = 'v' + manifest.version;
		}
	} catch (error) {
		// 版本号展示失败不影响功能
	}
})();

function showStatus(message, kind = '') {
	statusBox.textContent = message;
	statusBox.className = kind;
}

function setBusy(busy) {
	connectButton.disabled = busy;
	openButton.disabled = busy;
	transferButton.disabled = busy;
	openDouyinButton.disabled = busy;
	transferDouyinButton.disabled = busy;
	openTikTokButton.disabled = busy;
	transferTikTokButton.disabled = busy;
}

async function saveConfig() {
	await chrome.storage.local.set({
		serverUrl: serverInput.value.trim(),
		authToken: tokenInput.value
	});
}

async function loadConfig() {
	const config = await chrome.storage.local.get(['serverUrl', 'authToken']);
	serverInput.value = config.serverUrl || '';
	tokenInput.value = config.authToken || '';
}

async function captureDouyinSessionParams() {
	const tabs = await chrome.tabs.query({
		url: ['https://douyin.com/*', 'https://*.douyin.com/*']
	});
	const usable = tabs.filter((item) => item.id);
	if (usable.length === 0) {
		return { no_douyin_tab: true };
	}

	// 合并所有抖音标签页的采集结果：抖音 secsdk 密钥按“来源站（www.douyin.com）”
	// 存储，最近访问的标签页可能是 live/creator 等子域，没有 secure-store 的
	// IndexedDB；逐个执行并合并，取到密钥最多的结果作为最终会话。
	// 注意：不能依赖 awaitPromise / world=MAIN（部分浏览器如 Firefox 不支持，
	// 会报 Unexpected property: 'awaitPromise'），改为在页面隔离世界执行采集，
	// 通过 chrome.runtime.sendMessage 把结果回传给弹窗。
	const merged = {
		webid: '',
		verify_fp: '',
		ms_token: '',
		local_storage: {},
		ua: '',
		href: '',
		debug: []
	};
	for (const tab of usable) {
		const tabId = tab.id;
		const tabDebug = {
			url: tab.url || tab.pendingUrl || '',
			ok: false,
			error: '',
			local_storage_keys: 0,
			indexeddb_keys: 0
		};
		try {
			let messageReceived;
			{
				let resolveMessage;
				messageReceived = new Promise((resolve) => {
					resolveMessage = resolve;
				});
				const timer = setTimeout(() => {
					chrome.runtime.onMessage.removeListener(onMessage);
					resolveMessage(null);
				}, 10000);
				function onMessage(message, sender) {
					if (
						message &&
						message.type === 'douyin-secsdk-result' &&
						sender &&
						sender.tab &&
						sender.tab.id === tabId
					) {
						clearTimeout(timer);
						chrome.runtime.onMessage.removeListener(onMessage);
						resolveMessage(message.payload);
					}
				}
				chrome.runtime.onMessage.addListener(onMessage);
			}
			await chrome.scripting.executeScript({
				target: { tabId },
				func: () => {
					(async () => {
						const result = { debug: { local_storage_keys: 0, indexeddb_keys: 0 } };
						const urls = [
							...performance
								.getEntriesByType('resource')
								.map((entry) => entry.name)
								.reverse(),
							window.location.href
						];
						for (const value of urls) {
							try {
								const url = new URL(value);
								result.webid ||= url.searchParams.get('webid') || '';
								result.verify_fp ||=
									url.searchParams.get('verifyFp') || url.searchParams.get('fp') || '';
								result.ms_token ||= url.searchParams.get('msToken') || '';
								if (result.webid && result.verify_fp && result.ms_token) break;
							} catch {
								// 忽略无效或非 HTTP 资源地址。
							}
						}
						// 收集抖音 secsdk 会话密钥（localStorage + sessionStorage + IndexedDB 的
						// security-sdk/SLARDAR 等）。「我的喜欢」「收藏夹」接口签名需要这些密钥，
						// 服务端会写入 douyin-secsdk.json。
						const secsdkPrefix = /^(security-sdk\/|SLARDAR|web_runtime_security_uid|web_secsdk_runtime_cache|SysInfo|g_ven|web_secsdk_)/;
						const collectFromStorage = (storage) => {
							const collected = {};
							try {
								for (let i = 0; i < storage.length; i++) {
									const key = storage.key(i);
									if (key && secsdkPrefix.test(key)) {
										try {
											collected[key] = storage.getItem(key);
										} catch {
											// 单个键读取失败不影响其余密钥
										}
									}
								}
							} catch (error) {
								// storage 本身不可访问（如权限受限）时保留其余密钥
							}
							return collected;
						};
						const local_storage = {
							...collectFromStorage(localStorage),
							...collectFromStorage(sessionStorage)
						};
						result.debug.local_storage_keys = Object.keys(local_storage).length;
						// 抖音 secsdk 的部分密钥（s_sdk_crypt_sdk / s_sdk_server_cert_key 等）
						// 存在 IndexedDB（secure-store / douyin_secure_store 等）的 cryptvalues 库，
						// 需要一并读取并合并进 local_storage。自动发现数据库名，避免硬编码失效。
						try {
							const databases = await indexedDB.databases();
							const candidates = [
								'secure-store',
								'douyin_secure_store',
								'secsdk-store',
								'bytedance_secure_store'
							];
							for (const info of databases || []) {
								const name = info.name;
								if (!name || candidates.indexOf(name) === -1) continue;
								const openRequest = indexedDB.open(name);
								const db = await new Promise((resolve, reject) => {
									openRequest.onsuccess = () => resolve(openRequest.result);
									openRequest.onerror = () => reject(openRequest.error);
								});
								for (const storeName of db.objectStoreNames) {
									const store = db.transaction(storeName, 'readonly').objectStore(storeName);
									const keys = await new Promise((resolve, reject) => {
										const request = store.getAllKeys();
										request.onsuccess = () => resolve(request.result);
										request.onerror = () => reject(request.error);
									});
									const values = await new Promise((resolve, reject) => {
										const request = store.getAll();
										request.onsuccess = () => resolve(request.result);
										request.onerror = () => reject(request.error);
									});
									(keys || []).forEach((key, index) => {
										if (
											key &&
											values &&
											values[index] !== undefined &&
											secsdkPrefix.test(String(key))
										) {
											local_storage[String(key)] = String(values[index]);
										}
									});
								}
							}
						} catch (error) {
							result.debug.indexeddb_error = String(error && error.message ? error.message : error);
							// IndexedDB 读取失败时至少保留 localStorage/sessionStorage 中的密钥
						}
						result.debug.indexeddb_keys = Object.keys(local_storage).length - result.debug.local_storage_keys;
						if (result.debug.indexeddb_keys < 0) {
							result.debug.indexeddb_keys = 0;
						}
						if (Object.keys(local_storage).length > 0) {
							result.local_storage = local_storage;
						}
						result.ua = navigator.userAgent || '';
						result.href = window.location.href || '';
						try {
							chrome.runtime.sendMessage({
								type: 'douyin-secsdk-result',
								payload: result
							});
						} catch (e) {
							// 弹窗可能已关闭，结果直接丢弃
						}
					})().catch(() => {});
				}
			});
			const tabResult = await messageReceived;
			if (!tabResult) {
				tabDebug.error = '超时：页面采集脚本未回传结果';
			} else {
				tabDebug.ok = true;
				tabDebug.local_storage_keys = tabResult.debug?.local_storage_keys || 0;
				tabDebug.indexeddb_keys = tabResult.debug?.indexeddb_keys || 0;
				tabDebug.indexeddb_error = tabResult.debug?.indexeddb_error || '';
				if (tabResult.local_storage) {
					Object.assign(merged.local_storage, tabResult.local_storage);
				}
				merged.webid ||= tabResult.webid || '';
				merged.verify_fp ||= tabResult.verify_fp || '';
				merged.ms_token ||= tabResult.ms_token || '';
				merged.ua ||= tabResult.ua || '';
				merged.href ||= tabResult.href || '';
			}
		} catch (error) {
			// 单个标签页注入失败（例如页面权限受限）不影响其他标签页
			tabDebug.error = String(error && error.message ? error.message : error);
		}
		merged.debug.push(tabDebug);
	}
	if (Object.keys(merged.local_storage).length === 0) {
		delete merged.local_storage;
	}
	return merged;
}

connectButton.addEventListener('click', async () => {
	setBusy(true);
	try {
		const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
		if (!tab?.id) throw new Error('没有找到当前 Bili Sync 页面');
		const [{ result }] = await chrome.scripting.executeScript({
			target: { tabId: tab.id },
			func: () => ({
				serverUrl: window.location.origin,
				authToken: window.localStorage.getItem('auth_token') || ''
			})
		});
		if (!result?.authToken) {
			throw new Error('当前页面没有 Bili Sync API Token，请先在 Web 页面登录');
		}
		serverInput.value = normalizeServerUrl(result.serverUrl);
		tokenInput.value = result.authToken;
		await saveConfig();
		showStatus('已连接当前 Bili Sync 页面，可以打开 YouTube 登录。', 'success');
	} catch (error) {
		showStatus(error instanceof Error ? error.message : String(error), 'error');
	} finally {
		setBusy(false);
	}
});

openButton.addEventListener('click', async () => {
	await chrome.tabs.create({ url: 'https://www.youtube.com/' });
});

openDouyinButton.addEventListener('click', async () => {
	await chrome.tabs.create({ url: 'https://www.douyin.com/' });
});

openTikTokButton.addEventListener('click', async () => {
	await chrome.tabs.create({ url: 'https://www.tiktok.com/' });
});

async function transfer(platform) {
	setBusy(true);
	const douyin = platform === 'douyin';
	const tiktok = platform === 'tiktok';
	const label = tiktok ? 'TikTok' : douyin ? '抖音' : 'YouTube';
	showStatus(`正在读取并验证${label}登录状态…`);
	try {
		const serverUrl = normalizeServerUrl(serverInput.value);
		const authToken = tokenInput.value.trim();
		if (!authToken) throw new Error('请先连接 Bili Sync 设置页或填写 API Token');

		// 请求连接权限；服务端已支持 CORS，未授权时跨域请求也能成功，因此不强制阻断
		try {
			await chrome.permissions.request({
				origins: [permissionPattern(serverUrl)]
			});
		} catch {
			// 忽略权限请求异常，继续尝试跨域传输
		}

		const cookies = tiktok
			? [
					...(await chrome.cookies.getAll({ domain: 'tiktok.com' })),
					...(await chrome.cookies.getAll({ domain: 'tiktokcdn.com' }))
				]
			: douyin
				? [
						...(await chrome.cookies.getAll({ domain: 'douyin.com' })),
						...(await chrome.cookies.getAll({ domain: 'bytedance.com' }))
					]
				: [
						...(await chrome.cookies.getAll({ domain: 'youtube.com' })),
						...(await chrome.cookies.getAll({ domain: 'google.com' }))
					];
		const contents = buildNetscapeCookies(cookies, platform);
		const douyinSessionParams = douyin ? await captureDouyinSessionParams() : {};
		let douyinDiag = '';
		if (douyin) {
			const keyCount = Object.keys(douyinSessionParams.local_storage || {}).length;
			if (keyCount > 0) {
				douyinDiag = `抖音 secsdk 会话密钥已采集（${keyCount} 个）`;
			} else if (douyinSessionParams.no_douyin_tab) {
				douyinDiag = '没有找到已打开的抖音页面：请先点击“打开抖音”并在该页面登录，保持 www.douyin.com 首页打开后重试';
			} else {
				const rows = (douyinSessionParams.debug || [])
					.map(
						(entry) =>
							`${entry.url || '未知页面'}：${entry.ok ? '已注入' : '注入失败'}${entry.error ? '（' + entry.error + '）' : ''}，localStorage ${entry.local_storage_keys || 0} 个 / IndexedDB ${entry.indexeddb_keys || 0} 个${entry.indexeddb_error ? '，IndexedDB 错误：' + entry.indexeddb_error : ''}`
					)
					.join('；');
				douyinDiag = `未在抖音页面找到 secsdk 密钥（共 ${(douyinSessionParams.debug || []).length} 个抖音标签页：${rows}）。请确认已登录 www.douyin.com 且停留在抖音首页，再点一次“传输抖音登录状态”`;
			}
		}
		const controller = new AbortController();
		const timer = setTimeout(() => controller.abort(), 20000);
		let response;
		try {
			response = await fetch(`${serverUrl}/api/${platform}/cookies`, {
				method: 'POST',
				headers: {
					Authorization: authToken,
					'Content-Type': 'application/json'
				},
				body: JSON.stringify({ cookies: contents, ...douyinSessionParams }),
				signal: controller.signal
			});
		} catch (error) {
			if (error && error.name === 'AbortError') {
				throw new Error('传输超时（20 秒），请确认 Bili Sync 服务已启动后重试');
			}
			throw error;
		} finally {
			clearTimeout(timer);
		}
		const payload = await response.json().catch(() => null);
		if (!response.ok) {
			throw new Error(responseMessage(payload, `Bili Sync 返回 HTTP ${response.status}`));
		}

		serverInput.value = serverUrl;
		await saveConfig();
		const serverMessage = responseMessage(payload, '登录状态已传输到 Bili Sync');
		if (douyinDiag) {
			// 采集诊断与服务端结果合并展示，避免被成功提示覆盖
			showStatus(`${serverMessage}\n【采集诊断】${douyinDiag}`, douyinDiag.startsWith('抖音 secsdk') ? 'success' : 'error');
		} else {
			showStatus(serverMessage, 'success');
		}
	} catch (error) {
		showStatus(error instanceof Error ? error.message : String(error), 'error');
	} finally {
		setBusy(false);
	}
}

transferButton.addEventListener('click', async () => {
	await transfer('youtube');
});

transferDouyinButton.addEventListener('click', async () => {
	await transfer('douyin');
});

transferTikTokButton.addEventListener('click', async () => {
	await transfer('tiktok');
});

loadConfig().catch((error) => {
	showStatus(error instanceof Error ? error.message : String(error), 'error');
});
