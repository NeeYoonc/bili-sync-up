# Source-Level Retry for Charge Videos

## Goal

Add a manual source-level action that retries already-known charge videos for a single video source. The action should not periodically scan all sources and should not pre-check charge entitlement. It should simply place that source's existing charge videos back into the normal download pipeline.

## Scope

- Add a button on each video source card: "重试充电视频".
- Add a backend endpoint for the button:
  `POST /api/video-sources/{source_type}/{id}/retry-charge-videos`.
- Only affect videos belonging to the selected source.
- Only affect videos where `is_charge_video = true`.
- Preserve the existing download behavior:
  - if the account can now play the charge video, the normal download logic downloads real media;
  - if the account still cannot play it, the existing placeholder logic keeps or recreates the placeholder.

## Backend Design

The endpoint resolves `{source_type, id}` to the same source filter expressions used by existing source actions. It queries matching videos where:

- `valid = true`
- `deleted = 0`
- `auto_download = true`
- `is_charge_video = true`

For matched videos, it resets only the work needed to retry media download:

- video-level page-download status is set back to pending;
- page-level video-content download status is set back to pending;
- `auto_download` remains true;
- `valid` remains true.

The endpoint returns:

- whether anything was reset;
- number of videos reset;
- number of pages reset;
- a user-facing message.

If at least one video or page was reset, the endpoint triggers an immediate scan using the existing task controller, so the current downloader pipeline handles the actual retry.

## Frontend Design

The video source page adds a compact source action button using the existing button style and lucide icon pattern. The action:

- calls the new API method;
- disables itself while the request is in flight for that source;
- shows a success toast with the reset counts;
- shows the existing error toast flow if the API fails.

The button should be visible for source types that can contain charge videos. If the backend returns zero reset items, the toast says there are no charge videos to retry for that source.

## Error Handling

- Unknown source type or missing source returns the same API error style as existing source endpoints.
- If a scan is already running, the reset still happens and the normal scan loop will pick it up; if idle, the endpoint triggers a scan immediately.
- The endpoint does not delete placeholder files. Existing download code decides whether to replace them.

## Testing

Backend tests cover:

- only the selected source is affected;
- non-charge videos in the same source are unchanged;
- charge videos in other sources are unchanged;
- video page-download status and page video-content status are reset;
- response counts are correct.

Frontend verification covers:

- button renders in source card actions;
- clicking it calls the new API method;
- success and zero-count messages are shown.
