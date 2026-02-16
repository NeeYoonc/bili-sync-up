import { getGlossaryDescription } from './glossary';

const MAX_TEXT_LENGTH = 32;
const TARGET_SELECTOR = [
	'label',
	'[data-slot="label"]',
	'h1',
	'h2',
	'h3',
	'h4',
	'h5',
	'h6',
	'th',
	'span',
	'p',
	'option',
	'[role="tab"]',
	'[data-slot="tabs-trigger"]',
	'button',
	'a',
	'[data-glossary-term]'
].join(',');
const reportedUnknownTerms = new Set<string>();

function normalizeTextContent(text: string): string {
	return text.replace(/\s+/g, ' ').trim();
}

function resolveElementText(element: HTMLElement): string {
	const customTerm = element.getAttribute('data-glossary-term');
	if (customTerm) {
		return normalizeTextContent(customTerm);
	}

	const text = normalizeTextContent(element.textContent ?? '');
	if (text) return text;

	const title = normalizeTextContent(element.getAttribute('title') ?? '');
	return title;
}

function shouldSkipElement(element: HTMLElement): boolean {
	if (element.dataset.glossaryIgnore === 'true') return true;
	if (element.dataset.glossaryKeepTitle === 'true') return true;
	const existingTitle = element.getAttribute('title');
	if (existingTitle) {
		const normalizedTitle = normalizeTextContent(existingTitle);
		// 已有较长/结构化说明时保持原样，避免覆盖业务自定义提示
		if (normalizedTitle.length >= 30 || normalizedTitle.includes('\n') || normalizedTitle.includes('：')) {
			return true;
		}
	}
	return false;
}

function isMeaningfulTerm(term: string): boolean {
	if (!/[\u4e00-\u9fffA-Za-z]/.test(term)) return false;
	if (term.length < 2 || term.length > MAX_TEXT_LENGTH) return false;
	if (/[{}]/.test(term)) return false;
	if (/^\d+$/.test(term)) return false;
	if (/[。！？]/.test(term)) return false;
	if ((term.match(/\s+/g) ?? []).length > 7) return false;
	return true;
}

function applyTooltipToElement(element: HTMLElement): void {
	if (shouldSkipElement(element)) return;

	const term = resolveElementText(element);
	if (!term) return;
	if (!isMeaningfulTerm(term)) return;

	const description = getGlossaryDescription(term);
	if (!description) {
		// 可选调试：localStorage.setItem('glossary_debug', '1')
		if (typeof window !== 'undefined' && localStorage.getItem('glossary_debug') === '1') {
			if (!reportedUnknownTerms.has(term)) {
				reportedUnknownTerms.add(term);
				console.warn('[glossary] 未匹配词条：', term);
			}
		}
		return;
	}

	element.setAttribute('title', description);
	element.classList.add('cursor-help');
	element.dataset.glossaryAuto = 'true';

	// 将 Label 的说明同步到绑定控件，确保悬浮在输入框/下拉框本体时也能看到解释
	if (element instanceof HTMLLabelElement && element.htmlFor) {
		const target = document.getElementById(element.htmlFor);
		if (target instanceof HTMLElement) {
			const keep = target.dataset.glossaryKeepTitle === 'true';
			const hasTitle = !!target.getAttribute('title');
			if (!keep && !hasTitle) {
				target.setAttribute('title', description);
				target.classList.add('cursor-help');
				target.dataset.glossaryAuto = 'true';
			}
		}
	}
}

function scanRoot(root: ParentNode): void {
	const elements = root.querySelectorAll<HTMLElement>(TARGET_SELECTOR);
	for (const element of elements) {
		applyTooltipToElement(element);
	}
}

function scanNode(node: Node): void {
	if (!(node instanceof HTMLElement)) return;
	if (node.matches(TARGET_SELECTOR)) {
		applyTooltipToElement(node);
	}
	scanRoot(node);
}

export function installGlossaryTooltips(root: ParentNode = document): () => void {
	scanRoot(root);

	const observeTarget =
		root instanceof Document ? (root.body ?? root.documentElement) : (root as Node | null);
	if (!observeTarget) {
		return () => {};
	}

	const observer = new MutationObserver((mutations) => {
		for (const mutation of mutations) {
			for (const node of mutation.addedNodes) {
				scanNode(node);
			}
		}
	});

	observer.observe(observeTarget, {
		childList: true,
		subtree: true
	});

	return () => observer.disconnect();
}
