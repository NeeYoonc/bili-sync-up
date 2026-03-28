use once_cell::sync::Lazy;
use tokio::sync::watch;

struct ChangeNotifier {
    sender: watch::Sender<u64>,
}

impl ChangeNotifier {
    fn new() -> Self {
        let (sender, _) = watch::channel(0);
        Self { sender }
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.sender.subscribe()
    }

    fn notify(&self) {
        let next = (*self.sender.borrow()).wrapping_add(1);
        let _ = self.sender.send(next);
    }
}

static VIDEO_CHANGE_NOTIFIER: Lazy<ChangeNotifier> = Lazy::new(ChangeNotifier::new);
static VIDEO_SOURCE_CHANGE_NOTIFIER: Lazy<ChangeNotifier> = Lazy::new(ChangeNotifier::new);

pub fn subscribe_videos_changed() -> watch::Receiver<u64> {
    VIDEO_CHANGE_NOTIFIER.subscribe()
}

pub fn notify_videos_changed() {
    VIDEO_CHANGE_NOTIFIER.notify();
}

pub fn subscribe_video_sources_changed() -> watch::Receiver<u64> {
    VIDEO_SOURCE_CHANGE_NOTIFIER.subscribe()
}

pub fn notify_video_sources_changed() {
    VIDEO_SOURCE_CHANGE_NOTIFIER.notify();
}
