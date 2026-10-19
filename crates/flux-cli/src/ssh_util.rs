//! SSH batch p/ a VM (L7.3): fonte única dos flags fixos do modo batch e do
//! alvo — mudar timeout/auth é tocar UM ponto, não os 9 sites do CLI. Fora
//! de propósito: o `vm_ssh` (tokens.rs) é login interativo, quebraria com o
//! modo batch.

use std::process::Command;

/// `usuário@host` da VM (túnel, spec 09).
pub(crate) fn ssh_alvo() -> String {
    format!("{}@{}", crate::tunel::VM_USER, crate::tunel::VM_HOST)
}

/// `ssh` em modo batch com os flags fixos — ordem preservada (flags antes do
/// alvo, alvo antes do comando remoto). O site só passa o comando remoto e
/// configura o stdio (herdado no build, pipado no `env push`/overlay,
/// `.output()` no pull, `.status()` no live OFF).
pub(crate) fn ssh_vm(args: &[&str]) -> Command {
    let mut c = Command::new("ssh");
    c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8"]);
    c.arg(ssh_alvo());
    c.args(args);
    c
}
