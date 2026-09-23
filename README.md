# obs-dynamic-delay

Delay dinâmico para lives no OBS: o streamer **liga e desliga o delay quando quiser, com a live no ar**, sem derrubar a transmissão.

O "Stream Delay" nativo do OBS (Configurações > Avançado) só pode ser alterado com a live parada. Este projeto resolve isso com um relay local escrito em Rust que fica entre o OBS e a plataforma:

```
OBS ──RTMP──▶ obs-dynamic-delay (127.0.0.1:1935) ──RTMP/RTMPS──▶ Twitch / YouTube / Kick ...
                    ▲            ▲
          hotkeys (script Lua)   painel web / API HTTP (dock no OBS, Stream Deck)
```

## O que o público vê

| Ação | Efeito para quem assiste |
|---|---|
| **Ligar o delay** (ex.: 30 s) | A imagem congela no próximo keyframe (com áudio mudo) durante 30 s, enquanto o buffer enche. Depois a live continua normalmente, 30 s atrasada. A conexão com a plataforma nunca cai. |
| **Desligar o delay** | Corte seco para o presente: o trecho em buffer é descartado e a live volta a ficar ao vivo (com precisão de ~1 GOP, 2 s no padrão do OBS). |
| **Aumentar / diminuir** (±5 s, presets) | Mesma lógica: aumentar congela pela diferença, diminuir corta pela diferença. |
| **Parar a live no OBS com delay ligado** | O relay termina de enviar o trecho atrasado e só então encerra a live na plataforma. Para encerrar na hora, desligue o delay depois de parar. |

Nada é re-encodado: o relay só guarda e reenvia os pacotes que o OBS já codificou (CPU quase zero), reescrevendo os timestamps para a plataforma receber uma linha do tempo contínua. A memória usada é mais ou menos o bitrate vezes o delay (6 Mbps × 30 s ≈ 23 MB).

## Instalação

1. Baixe o `obs-dynamic-delay.exe` (aba Releases) ou compile com `cargo build --release`.
2. **Dê dois cliques nele.** O instalador:
   - espera o OBS fechar (o OBS regrava a configuração ao sair);
   - copia o relay e o script para `%APPDATA%\obs-dynamic-delay`;
   - importa o destino e a chave que estavam no OBS (ou pergunta, se não achar);
   - faz o OBS transmitir pelo relay (`rtmp://127.0.0.1:1935/live`) e desliga o Stream Delay nativo;
   - adiciona o script e o painel **Delay dinâmico** na tela do OBS;
   - guarda backup de tudo que alterou (`.dd-backup` e `obs-service-backup.json`).
3. Abra o OBS. O painel aparece como um dock (menu **Docks > Delay dinâmico**); arraste para onde quiser, por exemplo ao lado de "Controles".
4. Em **Configurações > Atalhos**, procure "Delay dinâmico" e defina as teclas.

Para atualizar ou desinstalar, dê dois cliques no exe de novo (ou `obs-dynamic-delay.exe --install` / `--uninstall`). A desinstalação remove o script e o painel e devolve a configuração de transmissão original.

### O painel "Delay dinâmico"

- **Estado e atraso real** para o público (AO VIVO / AJUSTANDO / DELAY).
- **Ligar/desligar**, presets (10, 30, 60, 120 s) e ±5 s.
- **Configuração:** plataforma (Twitch, YouTube, Kick, outra), URL, chave, "começar toda live com delay".
- **Configurar o OBS automaticamente** (aparece se o OBS não estiver apontando para o relay) e **Restaurar a configuração original**.
- Status do OBS e da conexão com a plataforma, com a mensagem de erro quando houver.

O relay abre escondido junto com o OBS (e também ao clicar em "Iniciar transmissão", se estiver fechado) e fecha junto com o OBS, depois de terminar de enviar o trecho atrasado. O log fica em `obs-dynamic-delay.log`, na pasta de instalação. Mudanças de destino ou chave durante a live valem a partir da próxima live.

### Como as peças conversam

- **Painel:** página local (`dock.html`) que fala com a API HTTP do relay.
- **Script Lua do OBS:** atalhos, abre e fecha o relay, e executa dentro do OBS o que o painel pede (trocar a configuração de transmissão), consultando o relay via UDP a cada segundo.
- **Relay:** guarda a configuração em `config.toml`.

### Sem o instalador

`obs-dynamic-delay.exe caminho\config.toml` roda só o relay (se o arquivo não existir, ele cria um comentado). O script `obs/obs-dynamic-delay.lua` pode ser adicionado à mão em Ferramentas > Scripts, com o exe na mesma pasta.

## API

Aceita GET e POST, então funciona direto no Stream Deck (ação "Website"), no Touch Portal, em bots etc.

| Rota | Ação |
|---|---|
| `/api/toggle` | liga/desliga |
| `/api/on` · `/api/off` | liga · desliga |
| `/api/delay/{s}` | define o delay em segundos |
| `/api/add/{s}` | soma (aceita negativo: `/api/add/-5`) |
| `/api/status` | estado em JSON |

Também há `GET/POST /api/config` (destino, chave, delay) e `POST /api/obs/configure` / `POST /api/obs/restore`.

A porta UDP (`8788`) aceita os mesmos comandos em texto: `toggle`, `on`, `off`, `set 30`, `add -5`, e também `status` (responde um resumo), `quit` (fecha depois que não houver live), `stay` (cancela o `quit`) e os comandos da ponte com o script (`poll`, `import`, `result`).

## Por que Rust e não Python

O trabalho pesado é um servidor + cliente RTMP que precisa segurar minutos de vídeo em memória e reenviar tudo no tempo certo, por horas, sem engasgar. Rust entrega isso num único `.exe` sem dependências, com uso de CPU desprezível e sem pausas de GC. Um script Python dentro do OBS não consegue interceptar o stream codificado, e exigiria que cada streamer configurasse um interpretador Python compatível com a versão do OBS. A parte que precisa rodar dentro do OBS (atalhos e ajustes nas configurações do OBS) ficou em Lua, que já vem embutido no OBS.

## Limitações

- Só RTMP/RTMPS (o padrão das plataformas). WHIP/SRT não passam pelo relay.
- Ligar o delay espera o próximo keyframe (até 2 s com o intervalo padrão), e desligar corta num keyframe.
- A imagem congelada repete um keyframe; com `filler_fps = 2` isso usa pouca banda. Se a plataforma reclamar de fps baixo, suba o valor.
- Testado de ponta a ponta localmente (ffmpeg como OBS e como plataforma, H.264 com B-frames + AAC). Antes de usar numa live importante, faça um teste numa live não listada na sua plataforma.

## Desenvolvimento

```sh
cargo test                  # testes do motor de delay, parser FLV, config
bash scripts/e2e-test.sh    # ponta a ponta com ffmpeg (precisa de ffmpeg e curl no PATH)
```

Estrutura:

- `src/engine.rs`: motor de delay (buffer, congelamento, corte, reescrita de timestamps)
- `src/ingest.rs`: servidor RTMP que recebe do OBS
- `src/upstream.rs`: cliente RTMP/RTMPS que publica na plataforma (reconecta sozinho)
- `src/control.rs` + `src/panel.html`: API HTTP, painel e porta UDP
- `src/installer.rs`: instalador (script, dock, configuração de transmissão do OBS)
- `obs/obs-dynamic-delay.lua`: script do OBS (configuração, auto-configuração do OBS, atalhos, gerenciamento do relay)
