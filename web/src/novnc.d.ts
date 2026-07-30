declare module '@novnc/novnc/lib/rfb.js' {
	export default class RFB {
		constructor(
			target: HTMLElement,
			url: string,
			options?: {
				shared?: boolean;
				credentials?: { password?: string };
			}
		);

		scaleViewport: boolean;
		resizeSession: boolean;
		viewOnly: boolean;
		background: string;

		disconnect(): void;
		focus(): void;
		addEventListener(type: string, listener: (event: Event) => void): void;
	}
}
