type QueryValue = string | number | boolean | null | undefined;

function getAuthToken(): string | null {
	if (typeof localStorage === 'undefined') {
		return null;
	}

	return localStorage.getItem('auth_token');
}

export function buildAuthenticatedStreamUrl(
	path: string,
	params: Record<string, QueryValue> = {}
): string | null {
	const token = getAuthToken();
	if (!token) {
		return null;
	}

	const searchParams = new URLSearchParams();
	for (const [key, value] of Object.entries(params)) {
		if (value === undefined || value === null) {
			continue;
		}
		searchParams.append(key, String(value));
	}
	searchParams.append('token', token);

	return `${path}?${searchParams.toString()}`;
}
