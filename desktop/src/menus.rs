//! Native menus use registered GPUI actions, including focused text-input actions.
use super::*;
use gpui::{Menu, MenuItem, OsAction, SystemMenuType};
actions!(
    desktop_menu,
    [
        Settings,
        About,
        ChooseFile,
        ChooseFolder,
        Close,
        Minimize,
        Zoom,
        Fullscreen,
        Hide,
        HideOthers,
        ShowAll,
        Help
    ]
);

pub fn install(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Help, cx| cx.open_url("https://github.com/cdenihan/XFER#readme"));
    cx.set_menus(vec![
        Menu {
            name: "XFER".into(),
            items: vec![
                MenuItem::action("About XFER", About),
                MenuItem::action("Settings…", Settings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide XFER", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit XFER", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Select File…", ChooseFile),
                MenuItem::action("Select Folder…", ChooseFolder),
                MenuItem::separator(),
                MenuItem::action("Close Window", Close),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::separator(),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
                MenuItem::action("Emoji & Symbols", input::ShowCharacterPalette),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::action("Toggle Full Screen", Fullscreen),
            ],
        },
        Menu {
            name: "Help".into(),
            items: vec![MenuItem::action("XFER Help", Help)],
        },
    ]);
}

impl Desktop {
    pub(super) fn menu_settings(
        &mut self,
        _: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switch(View::Settings, None, cx);
        window.focus(&self.focus);
    }
    pub(super) fn menu_about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        self.switch(View::Settings, None, cx);
        window.focus(&self.focus);
    }
    pub(super) fn menu_file(&mut self, _: &ChooseFile, _: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_none() && self.retiring.is_none() {
            self.switch(View::Workflow, Some(Action::Copy), cx);
            self.choose(false, cx);
        }
    }
    pub(super) fn menu_folder(&mut self, _: &ChooseFolder, _: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_none() && self.retiring.is_none() {
            self.switch(View::Workflow, None, cx);
            self.choose(true, cx);
        }
    }
    pub(super) fn menu_close(&mut self, _: &Close, _: &mut Window, cx: &mut Context<Self>) {
        self.shutdown(cx);
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    #[gpui::test]
    fn native_actions_route_through_focused_input_and_root(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| Desktop::new(None, cx));
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.ready = true;
                this.inputs[0].update(cx, |input, cx| input.set("résumé.txt".into(), cx));
                window.focus(&this.inputs[0].focus_handle(cx));
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(window.is_action_available(&input::Copy, cx));
            assert!(window.is_action_available(&Settings, cx));
            assert!(window.is_action_available(&Minimize, cx));
        });
        cx.dispatch_action(input::SelectAll);
        cx.dispatch_action(input::Copy);
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "résumé.txt"
            )
        });
        cx.dispatch_action(Settings);
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(view.read(cx).view == View::Settings);
            assert!(view.read(cx).focus.is_focused(window));
            assert!(!window.is_action_available(&input::Paste, cx));
        });
        cx.dispatch_action(About);
        cx.run_until_parked();
        view.update(cx, |this, _| assert!(this.view == View::Settings));
    }
}
