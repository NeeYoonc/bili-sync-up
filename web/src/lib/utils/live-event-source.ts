type EventHandler = (event: MessageEvent, eventSource: EventSource) => void;

type ManagedEventSourceOptions = {
	url: string | null;
	handlers?: Record<string, EventHandler>;
	onError?: (eventSource: EventSource) => void;
	onStop?: () => void;
};

export function createManagedEventSource() {
	let eventSource: EventSource | null = null;
	let currentUrl: string | null = null;

	function isCurrent(source: EventSource) {
		return eventSource === source;
	}

	function stop() {
		if (eventSource) {
			eventSource.close();
			eventSource = null;
		}
		currentUrl = null;
	}

	function reset() {
		stop();
	}

	function start({ url, handlers = {}, onError, onStop }: ManagedEventSourceOptions) {
		if (!url) {
			stop();
			onStop?.();
			return false;
		}

		if (eventSource && currentUrl === url) {
			return false;
		}

		stop();
		onStop?.();
		currentUrl = url;

		const source = new EventSource(url);
		eventSource = source;

		for (const [eventName, handler] of Object.entries(handlers)) {
			source.addEventListener(eventName, (event) => {
				handler(event as MessageEvent, source);
			});
		}

		source.onerror = () => {
			if (!isCurrent(source)) return;
			onError?.(source);
		};

		return true;
	}

	return {
		start,
		stop,
		reset,
		isCurrent,
		getCurrentUrl: () => currentUrl
	};
}
