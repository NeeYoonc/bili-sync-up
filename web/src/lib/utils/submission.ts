import { formatCompactTimestampOrFallback } from './timezone';

export function formatSubmissionDateLabel(pubtime: string): string {
	return formatCompactTimestampOrFallback(pubtime, 'Asia/Shanghai', pubtime);
}

export function formatSubmissionMetricLabel(count: number): string {
	if (count >= 10000) {
		return `${(count / 10000).toFixed(1)}万`;
	}

	return count.toString();
}
