import { formatTimestampOrFallback } from './timezone';

export function formatSubmissionDateLabel(pubtime: string): string {
	return formatTimestampOrFallback(pubtime, 'Asia/Shanghai', 'date', pubtime);
}

export function formatSubmissionMetricLabel(count: number): string {
	if (count >= 10000) {
		return `${(count / 10000).toFixed(1)}万`;
	}

	return count.toString();
}
