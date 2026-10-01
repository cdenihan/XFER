use gpui::{AssetSource, SharedString};
use std::borrow::Cow;
pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "home" => Some(Cow::Borrowed(include_bytes!("../assets/ui/home.svg"))),
            "transfer" => Some(Cow::Borrowed(include_bytes!("../assets/ui/transfer.svg"))),
            "sync" => Some(Cow::Borrowed(include_bytes!("../assets/ui/sync.svg"))),
            "shield" => Some(Cow::Borrowed(include_bytes!("../assets/ui/shield.svg"))),
            "settings" => Some(Cow::Borrowed(include_bytes!("../assets/ui/settings.svg"))),
            "folder" => Some(Cow::Borrowed(include_bytes!("../assets/ui/folder.svg"))),
            "computer" => Some(Cow::Borrowed(include_bytes!("../assets/ui/computer.svg"))),
            "send" => Some(Cow::Borrowed(include_bytes!("../assets/ui/send.svg"))),
            "receive" => Some(Cow::Borrowed(include_bytes!("../assets/ui/receive.svg"))),
            "wifi" => Some(Cow::Borrowed(include_bytes!("../assets/ui/wifi.svg"))),
            "link" => Some(Cow::Borrowed(include_bytes!("../assets/ui/link.svg"))),
            "close" => Some(Cow::Borrowed(include_bytes!("../assets/ui/close.svg"))),
            "check" => Some(Cow::Borrowed(include_bytes!("../assets/ui/check.svg"))),
            _ => None,
        })
    }
    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
