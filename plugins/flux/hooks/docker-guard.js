#!/usr/bin/env node
/**
 * docker-guard.js — hook PreToolUse (matcher Bash) do plugin flux.
 *
 * Comando que começa com `docker` no host = erro de rota: nesta máquina o
 * Docker vive DENTRO da VM Debian (192.168.140.2) e é operado pelo CLI
 * `flux` (ou pelas tools MCP flux_*). O host Windows nem tem Docker. O hook
 * RECUSA o comando (exit 2) com a mensagem no stderr — o agente lê a causa
 * e troca para o caminho do flux sozinho.
 *
 * Protocolo: entrada JSON no stdin (tool_input.command); saída NADA no
 * stdout (é canal de protocolo — só stderr na recusa). Exit 0 = passa.
 */

const fs = require("fs");

function comandoDe(raw) {
  try {
    const msg = JSON.parse(raw);
    const ti = msg.tool_input ?? msg.input ?? {};
    const cmd = ti.command ?? ti.cmd ?? "";
    return typeof cmd === "string" ? cmd : "";
  } catch {
    return "";
  }
}

const cmd = comandoDe(fs.readFileSync(0, "utf-8"));

if (/^\s*docker\b/i.test(cmd)) {
  console.error(
    "[flux] O padrão deste PC é a plataforma Flux: Docker roda DENTRO da VM " +
      "Debian (192.168.140.2) — o host Windows nem tem Docker. Nada de " +
      "docker ps/compose/exec direto. Use o CLI `flux` (flux up, flux ps, " +
      "flux logs, flux exec, flux db status) ou as tools MCP flux_* " +
      "(flux_status, flux_ps, flux_acordar, flux_db_query, flux_tool). " +
      "SSH direto na VM (flux vm ssh) só para o que o flux não cobre."
  );
  process.exit(2);
}

process.exit(0);
