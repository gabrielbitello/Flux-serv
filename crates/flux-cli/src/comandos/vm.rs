//! vm (F1 parcial): ciclo de energia da VM VirtualBox NO HOST Windows.
//! Fonte única de "como ligar/desligar a VM certo" — o VBoxManage nunca é
//! chamado na mão nem por script externo; a task `FluxVMAutostart` do logon
//! chama ESTE comando. Só faz sentido no host: dentro da VM recusa com causa.

/// Caminho canônico do VBoxManage (fora do PATH do Git Bash — armadilha
/// mordida no recovery de 06/11).
const VBOXMANAGE: &str = "C:\\Program Files\\Oracle\\VirtualBox\\VBoxManage.exe";
/// Nome da VM registrada no VirtualBox (`E:\flux\vms\Flux`).
const VM_NOME: &str = "Flux";
/// Teto do desligar graceful: ACPI ignorado (guest wedged) = erro orientando
/// `--forcar` em vez de CLI pendurado.
const TETO_GRACEFUL_SECS: u64 = 120;

use std::process::Command;

#[derive(Debug, PartialEq)]
enum Estado {
    Desligada,
    NoAr,
    Outro(String),
}

/// Estado pelo `showvminfo --machinereadable` — a única linha que importa é
/// `VMState="x"`.
fn estado_da_saida(saida: &str) -> Estado {
    let valor = saida
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("VMState=\""))
        .map(|s| s.trim_end_matches('"').to_string());
    match valor.as_deref() {
        Some("poweroff") => Estado::Desligada,
        Some("running") => Estado::NoAr,
        Some(outro) => Estado::Outro(outro.to_string()),
        None => Estado::Outro("sem VMState na saída do VBoxManage".to_string()),
    }
}

fn vboxmanage() -> std::process::Command {
    if !std::path::Path::new(VBOXMANAGE).exists() {
        eprintln!(
            "erro: VBoxManage não está em {VBOXMANAGE} — `flux vm` roda NO HOST \
             Windows com o VirtualBox instalado; dentro da VM não faz sentido"
        );
        std::process::exit(1);
    }
    Command::new(VBOXMANAGE)
}

/// Estado atual da VM registrada. Erro com causa quando a VM nem existe
/// (nome mudou, VirtualBox reinstalado).
fn estado() -> Estado {
    let saida = vboxmanage()
        .args(["showvminfo", VM_NOME, "--machinereadable"])
        .output()
        .unwrap_or_else(|e| {
            eprintln!("erro: VBoxManage não lançado: {e}");
            std::process::exit(1);
        });
    if !saida.status.success() {
        let causa = String::from_utf8_lossy(&saida.stderr);
        eprintln!("erro: showvminfo {VM_NOME} falhou: {}", causa.trim());
        std::process::exit(1);
    }
    estado_da_saida(&String::from_utf8_lossy(&saida.stdout))
}

/// Espera até `alvo` (cap 20 s) — o startvm volta antes do estado mudar.
fn espera_estado(alvo: &Estado) -> bool {
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if &estado() == alvo {
            return true;
        }
    }
    false
}

fn rodar(args: &[&str]) -> bool {
    let saida = vboxmanage().args(args).output().unwrap_or_else(|e| {
        eprintln!("erro: VBoxManage não lançado: {e}");
        std::process::exit(1);
    });
    if !saida.status.success() {
        eprintln!(
            "erro: VBoxManage {} falhou: {}",
            args.join(" "),
            String::from_utf8_lossy(&saida.stderr).trim()
        );
        return false;
    }
    true
}

/// startvm headless + espera o `running` — caminho comum de poweroff, saved
/// e aborted.
fn iniciar_headless() -> ! {
    if !rodar(&["startvm", VM_NOME, "--type", "headless"]) {
        std::process::exit(1);
    }
    if espera_estado(&Estado::NoAr) {
        println!(
            "VM {VM_NOME} no ar. SSH responde em ~1 min; containers acordam \
             por request/lease (`flux up`) — não é preciso ligar nada à mão."
        );
        std::process::exit(0);
    }
    eprintln!(
        "erro: startvm aceito mas o estado não virou `running` em 20 s — confira `flux vm status`"
    );
    std::process::exit(1);
}

/// `flux vm start` — liga a VM em modo headless. Idempotente: já no ar é
/// sucesso (a task de autostart roda isto em todo logon).
pub(crate) fn start() -> ! {
    match estado() {
        Estado::NoAr => {
            println!("VM {VM_NOME} já no ar — nada a fazer.");
            std::process::exit(0);
        }
        Estado::Desligada => iniciar_headless(),
        Estado::Outro(e) if matches!(e.as_str(), "saved" | "aborted") => iniciar_headless(),
        Estado::Outro(e) => {
            eprintln!(
                "erro: VM {VM_NOME} em estado `{e}` — não vou adivinhar. \
                 Paused = retomar no hipervisor; wedged = playbook de recovery \
                 (poweroff seco + startvm), que é exatamente `flux vm stop --forcar` + `flux vm start`."
            );
            std::process::exit(1);
        }
    }
}

/// `flux vm stop [--forcar]` — graceful primeiro (ACPI); corte seco só com
/// `--forcar` (é o playbook de recovery quando o guest ignora ACPI, mas
/// mata sem flush). Desligar a VM derruba TUDO: containers, zcode-api, rádio.
pub(crate) fn stop(forcar: bool) -> ! {
    match estado() {
        Estado::Desligada => {
            println!("VM {VM_NOME} já desligada — nada a fazer.");
            std::process::exit(0);
        }
        Estado::NoAr | Estado::Outro(_) => {
            if forcar {
                println!(
                    "CORTE SECO na VM {VM_NOME} (sem flush — recovery). Tudo dentro cai agora."
                );
                let ok = rodar(&["controlvm", VM_NOME, "poweroff"]);
                std::process::exit(if ok { 0 } else { 1 });
            }
            println!(
                "Desligando a VM {VM_NOME} gracefully (ACPI) — containers, zcode-api e rádio caem junto."
            );
            if !rodar(&["controlvm", VM_NOME, "acpipowerbutton"]) {
                std::process::exit(1);
            }
            if espera_estado(&Estado::Desligada) {
                println!("VM {VM_NOME} desligada limpa.");
                std::process::exit(0);
            }
            eprintln!(
                "erro: ACPI não concluiu em {TETO_GRACEFUL_SECS}s — guest provavelmente wedged \
                 (ignora ACPI). Corte seco é o playbook de recovery e pode corromper o disco: \
                 só com `flux vm stop --forcar`."
            );
            std::process::exit(1);
        }
    }
}

/// `flux vm status` — estado no hipervisor, sem tocar em nada.
pub(crate) fn status() -> ! {
    let texto = match estado() {
        Estado::NoAr => "no ar (running)".to_string(),
        Estado::Desligada => "desligada (poweroff)".to_string(),
        Estado::Outro(e) => format!("estado `{e}`"),
    };
    println!("VM {VM_NOME}: {texto}");
    println!("Autostart: task `FluxVMAutostart` (logon +90 s) chama `flux vm start`.");
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estado_interpreta_showvminfo() {
        assert_eq!(
            estado_da_saida("name=\"Flux\"\nVMState=\"running\"\nmemory=6644"),
            Estado::NoAr
        );
        assert_eq!(estado_da_saida("VMState=\"poweroff\""), Estado::Desligada);
        assert_eq!(
            estado_da_saida("VMState=\"paused\""),
            Estado::Outro("paused".to_string())
        );
        assert_eq!(
            estado_da_saida("sem estado aqui"),
            Estado::Outro("sem VMState na saída do VBoxManage".to_string())
        );
    }
}
