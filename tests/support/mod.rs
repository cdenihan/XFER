use std::{
    ffi::OsStr,
    path::Path,
    process::{Command as ProcessCommand, Output},
};
pub struct Command(ProcessCommand);
impl Command {
    pub fn cargo_bin(_: &str) -> Self {
        Self(ProcessCommand::new(env!("CARGO_BIN_EXE_xfer")))
    }
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.0.arg(arg);
        self
    }
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }
    pub fn current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.0.current_dir(path);
        self
    }
    pub fn get_program(&self) -> &OsStr {
        self.0.get_program()
    }
    pub fn output(&mut self) -> std::io::Result<Output> {
        self.0.output()
    }
    pub fn assert(&mut self) -> Assertion {
        Assertion(self.0.output().unwrap())
    }
}
pub struct Assertion(Output);
impl Assertion {
    pub fn success(self) -> Self {
        assert!(
            self.0.status.success(),
            "{}",
            String::from_utf8_lossy(&self.0.stderr)
        );
        self
    }
    pub fn failure(self) -> Self {
        assert!(!self.0.status.success());
        self
    }
    pub fn stdout(self, check: impl Fn(&str) -> bool) -> Self {
        let text = String::from_utf8_lossy(&self.0.stdout);
        assert!(check(&text), "unexpected stdout: {text}");
        self
    }
    pub fn stderr(self, check: impl Fn(&str) -> bool) -> Self {
        let text = String::from_utf8_lossy(&self.0.stderr);
        assert!(check(&text), "unexpected stderr: {text}");
        self
    }
}
pub fn contains(value: impl Into<String>) -> impl Fn(&str) -> bool {
    let value = value.into();
    move |text| text.contains(&value)
}
pub fn equals(value: impl Into<String>) -> impl Fn(&str) -> bool {
    let value = value.into();
    move |text| text == value
}
