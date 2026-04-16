// 时区转换工具函数 - 统一使用北京时间

// 固定使用北京时间
const BEIJING_TIMEZONE = 'Asia/Shanghai';

function isBlankTimestamp(timestamp: string | number | Date | null | undefined): boolean {
	return timestamp === null || timestamp === undefined || timestamp === '';
}

// 格式化时间戳到北京时间
// eslint-disable-next-line @typescript-eslint/no-unused-vars
// eslint-disable-next-line @typescript-eslint/no-unused-vars
export function formatTimestamp(
	timestamp: string | number | Date,
	_timezone: string = BEIJING_TIMEZONE,
	format: 'datetime' | 'date' | 'time' = 'datetime'
): string {
	// 标记参数已使用（保持API兼容且不改变行为）
	void _timezone;
	try {
		let date: Date;

		if (typeof timestamp === 'string') {
			// 处理字符串时间戳
			date = new Date(timestamp);
		} else if (typeof timestamp === 'number') {
			// 处理数字时间戳（秒或毫秒）
			date = new Date(timestamp < 1e12 ? timestamp * 1000 : timestamp);
		} else {
			date = timestamp;
		}

		// 检查日期是否有效
		if (isNaN(date.getTime())) {
			return '无效时间';
		}

		const options: Intl.DateTimeFormatOptions = {
			timeZone: BEIJING_TIMEZONE, // 始终使用北京时间
			year: 'numeric',
			month: '2-digit',
			day: '2-digit',
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit',
			hour12: false
		};

		switch (format) {
			case 'date':
				delete options.hour;
				delete options.minute;
				delete options.second;
				break;
			case 'time':
				delete options.year;
				delete options.month;
				delete options.day;
				break;
		}

		return new Intl.DateTimeFormat('zh-CN', options).format(date);
	} catch (error) {
		console.error('时间格式化失败:', error);
		return '格式化失败';
	}
}

export function isInvalidFormattedTime(value: string): boolean {
	return value === '无效时间' || value === '格式化失败';
}

export function formatTimestampOrFallback(
	timestamp: string | number | Date | null | undefined,
	_timezone: string = BEIJING_TIMEZONE,
	format: 'datetime' | 'date' | 'time' = 'datetime',
	fallback?: string
): string {
	if (isBlankTimestamp(timestamp)) {
		return fallback ?? '';
	}

	const formatted = formatTimestamp(timestamp as string | number | Date, _timezone, format);
	if (isInvalidFormattedTime(formatted)) {
		return fallback ?? String(timestamp);
	}

	return formatted;
}

// 获取相对时间描述
// eslint-disable-next-line @typescript-eslint/no-unused-vars
// eslint-disable-next-line @typescript-eslint/no-unused-vars
export function getRelativeTime(
	timestamp: string | number | Date,
	_timezone: string = BEIJING_TIMEZONE
): string {
	// 标记参数已使用（保持API兼容且不改变行为）
	void _timezone;
	try {
		let date: Date;

		if (typeof timestamp === 'string') {
			date = new Date(timestamp);
		} else if (typeof timestamp === 'number') {
			date = new Date(timestamp < 1e12 ? timestamp * 1000 : timestamp);
		} else {
			date = timestamp;
		}

		if (isNaN(date.getTime())) {
			return '无效时间';
		}

		const now = new Date();
		const diffMs = now.getTime() - date.getTime();
		const diffSeconds = Math.floor(diffMs / 1000);
		const diffMinutes = Math.floor(diffSeconds / 60);
		const diffHours = Math.floor(diffMinutes / 60);
		const diffDays = Math.floor(diffHours / 24);

		if (diffSeconds < 60) {
			return '刚刚';
		} else if (diffMinutes < 60) {
			return `${diffMinutes}分钟前`;
		} else if (diffHours < 24) {
			return `${diffHours}小时前`;
		} else if (diffDays < 7) {
			return `${diffDays}天前`;
		} else {
			// 超过一周显示具体日期
			return formatTimestamp(date, BEIJING_TIMEZONE, 'datetime');
		}
	} catch (error) {
		console.error('相对时间计算失败:', error);
		return '计算失败';
	}
}

// 转换UTC时间到北京时间
export function convertUTCToTimezone(
	utcTimestamp: string | number | Date,
	_timezone: string = BEIJING_TIMEZONE // eslint-disable-line @typescript-eslint/no-unused-vars
): Date {
	let date: Date;

	if (typeof utcTimestamp === 'string') {
		// 如果字符串不包含时区信息，假设为UTC
		if (!utcTimestamp.includes('Z') && !utcTimestamp.includes('+') && !utcTimestamp.includes('-')) {
			date = new Date(utcTimestamp + 'Z');
		} else {
			date = new Date(utcTimestamp);
		}
	} else if (typeof utcTimestamp === 'number') {
		date = new Date(utcTimestamp < 1e12 ? utcTimestamp * 1000 : utcTimestamp);
	} else {
		date = utcTimestamp;
	}

	return date;
}

// 获取时区偏移信息 - 北京时间固定为 UTC+08:00
// eslint-disable-next-line @typescript-eslint/no-unused-vars
export function getTimezoneOffset(_timezone: string = BEIJING_TIMEZONE): string {
	// 标记参数已使用（保持API兼容且不改变行为）
	void _timezone;
	return 'UTC+08:00';
}
