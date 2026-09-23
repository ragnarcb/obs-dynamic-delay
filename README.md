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

Tudo é feito de dentro do OBS, sem editar arquivo nenhum.

1. Baixe o zip da aba Releases (ou rode `cargo build --release` e junte `target/release/obs-dynamic-delay.exe` com `obs/obs-dynamic-delay.lua` numa pasta). Deixe a pasta num lugar fixo.
2. No OBS: **Ferramentas > Scripts > +** e escolha `obs-dynamic-delay.lua` dessa pasta. Não precisa instalar Python, o OBS já traz o LuaJIT.
3. Nas propriedades do script, clique em **Configurar o OBS automaticamente**. Ele:
   - importa a plataforma e a chave que já estavam configuradas no OBS;
   - troca o destino do OBS para o relay local (`rtmp://127.0.0.1:1935/live`);
   - desliga o Stream Delay nativo do OBS;
   - guarda a configuração original (o botão **Restaurar configuração original do OBS** desfaz tudo).
4. **Configurações > Atalhos**: procure "Delay dinâmico" e defina as teclas (ligar/desligar, ligar, desligar, aumentar, diminuir).

O relay abre escondido quando o OBS abre (e também ao clicar em "Iniciar transmissão", se estiver fechado) e fecha junto com o OBS, depois de terminar de enviar o trecho atrasado. O log fica em `obs-dynamic-delay.log`, na mesma pasta.

### Propriedades do script

| Grupo | O que tem |
|---|---|
| Status | estado do delay, atraso atual, conexão com a plataforma, se o OBS está configurado; botões "Atualizar status" e "Ligar/desligar delay agora" |
| Destino da live | plataforma (Twitch, YouTube, Kick, outra), URL, chave, configurar/restaurar o OBS |
| Delay | segundos (muda na hora, até durante a live), começar toda live com delay, passo do aumentar/diminuir, abrir o painel no navegador |
| Avançado | iniciar/fechar o relay junto com o OBS, reiniciar relay, caminho do executável, portas |

Mudanças de destino, chave ou portas feitas durante a live valem a partir da próxima live.

### Dock (opcional)

**Docks > Docks de navegador personalizados**: nome "Delay", URL `http://127.0.0.1:8787/`. Mostra AO VIVO / AJUSTANDO / DELAY e o atraso em segundos, com botões.

### Sem o script

O relay também roda sozinho: `obs-dynamic-delay.exe [config.toml]`. Se o arquivo não existir, ele cria um com os comentários explicando cada opção.

## API

Aceita GET e POST, então funciona direto no Stream Deck (ação "Website"), no Touch Portal, em bots etc.

| Rota | Ação |
|---|---|
| `/api/toggle` | liga/desliga |
| `/api/on` · `/api/off` | liga · desliga |
| `/api/delay/{s}` | define o delay em segundos |
| `/api/add/{s}` | soma (aceita negativo: `/api/add/-5`) |
| `/api/status` | estado em JSON |

A porta UDP (`8788`) aceita os mesmos comandos em texto: `toggle`, `on`, `off`, `set 30`, `add -5`, e também `status` (responde um resumo), `quit` (fecha depois que não houver live) e `stay` (cancela o `quit`).

## Por que Rust e não Python

O trabalho pesado é um servidor + cliente RTMP que precisa segurar minutos de vídeo em memória e reenviar tudo no tempo certo, por horas, sem engasgar. Rust entrega isso num único `.exe` sem dependências, com uso de CPU desprezível e sem pausas de GC. Um script Python dentro do OBS não consegue interceptar o stream codificado, e exigiria que cada streamer configurasse um interpretador Python compatível com a versão do OBS. A parte que precisa rodar dentro do OBS (os atalhos) ficou em Lua, que já vem embutido no OBS.

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
- `obs/obs-dynamic-delay.lua`: script do OBS (configuração, auto-configuração do OBS, atalhos, gerenciamento do relay)
