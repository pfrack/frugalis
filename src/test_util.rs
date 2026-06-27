#[cfg(test)]
/// Test-only guard for environment variables.
/// Sets an env var on construction (via `EnvGuard::set`) and removes it on drop.
/// Usage pattern: `let _guard = EnvGuard("VAR_NAME"); std::env::set_var("VAR_NAME", "value");`
pub struct EnvGuard(pub &'static str);

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}
