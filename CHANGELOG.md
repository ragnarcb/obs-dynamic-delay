# Changelog

Formato baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/); versões seguem [SemVer](https://semver.org/lang/pt-BR/).

## [0.1.0] - 2026-09-23

Primeira versão.

### Adicionado

- Relay RTMP local entre o OBS e a plataforma, com delay que liga, desliga e muda de tamanho durante a live.
  - Aumentar o delay congela no próximo quadro-chave, com áudio AAC mudo, até o buffer encher.
  - Diminuir corta no quadro-chave mais recente.
  - Timestamps contínuos, inclusive com B-frames; suporte a H.264, HEVC/AV1 (Enhanced RTMP) e AAC.
- Envio para RTMP e RTMPS (Twitch, YouTube, Kick e outros), com reconexão automática.
- Painel "Delay dinâmico" como dock no OBS: estado, atraso real, presets, configuração de destino e chave.
- Script Lua para o OBS: atalhos, abre e fecha o relay junto com o OBS, configura e restaura a transmissão.
- Instalador com dois cliques: importa destino e chave, configura o OBS, desliga o delay nativo, adiciona script e dock, com backups; opção de desinstalar.
- API HTTP (Stream Deck, bots) e comandos UDP.
- Página de relógio (`/clock`) para medir o delay na live.
