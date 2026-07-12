# Plano — Trazer as features do editor do ZenNotes para os blocos do momor

## STATUS (2026-07-09) — o que já foi implementado

| Fase | Status | Nota |
|---|---|---|
| **0 — Fundação SVG→img** | ✅ FEITO | Reusa o rasterizador nativo do gpui (`cx.svg_renderer().render_single_frame`). Cache thread-local por hash. Sem deps novas pro pipeline. |
| **2 — Function-plot** | ✅ FEITO | ` ```plot ` → gráfico de função (`fasteval` → SVG, 100% Rust). |
| **1 — Math/equações** | ✅ FEITO | ` ```math ` → LaTeX real via `mathjax_svg` (MathJax embutido, offline, SVG de paths). Prioridade nº1 do usuário. |
| **Bônus — auto-detecção markdown** | ✅ FEITO | `# `/`- `/`> `/`N. `/`- [ ]`/`---` convertem na hora; ` ```lang ` + Enter vira bloco. |
| **Bônus — fix backspace c/ seleção** | ✅ FEITO | `merge_backward` só funde sem seleção. |
| **3 — Mermaid** | ⛔ BLOQUEADO nativo | Verificado: não há renderer mermaid offline-puro em Rust — `mermaid-rs` puxa `auto_generate_cdp` (Chrome headless); a alternativa é `mmdc` (node). Precisa de **sidecar externo + render async**. É um follow-up próprio. |
| **4 — TikZ / Excalidraw** | ⛔ DEFERIDO | TikZ = toolchain LaTeX; Excalidraw = app de canvas JS. Desproporcionais p/ um slice nativo. |
| **5 — Features de conhecimento** | 🟡 EM PROGRESSO | **FEITO**: wikilinks (Cmd+clique abre; `[[X]]`-só vira BADGE com ícone), backlinks (rodapé "Referências"), drag nota/reunião/pasta→`[[wikilink]]`, **WYSIWYG** (Paragraph/Quote/Headings fora de foco renderizam markdown de verdade via crate `markdown` — negrito/código/heading/callout, marcadores escondidos; clique edita), realce markdown nas listas, menu do `+` pré-seleciona o tipo atual. **Falta**: autocomplete `[[`, wikilink badge inline (dentro de texto misto), outline, tasks agregadas, toolbar de seleção, **tabelas ricas** (ver abaixo). |

### Tabelas ricas (D) — plano do próximo passo dedicado

É o maior pedaço restante — essencialmente um **mini-database** (não uma tabela markdown simples). Escopo:
1. **v1 (bounded)**: `BlockKind::Table` — bloco multi-linha (Enter=Newline reusando o contexto "NoteCode"), edita markdown `| a | b |`, **renderiza como tabela** via `MarkdownElement` fora de foco (o crate `markdown` já renderiza GFM tables — de graça). `/table` no menu insere starter. `parse_blocks` agrupa linhas `|…|` (round-trip). Botões **+Linha/+Coluna** (manipulam o markdown cru).
2. **v2 (database)**: **filtros**, ordenação, tipos de coluna — precisa de modelo estruturado (linhas/colunas com estado), não markdown. É o CSV-database do ZenNotes. Store próprio.

Por que não foi feito junto: são ~8 pontos de toque (enum, block_style, MENU, parse/serialize, keymap, render, add-row/col) + os filtros são database-scale. Merece um turno focado com verificação interativa, não cramado no fim de um turno gigante.

**Resumo:** as duas features NOMEADAS como prioridade (equações + gráficos) estão prontas e nativas. Mermaid/TikZ/Excalidraw são os únicos que exigem ferramenta externa (browser/LaTeX/canvas — comprovado). Fase 5 (KM) é nativa e é o próximo passo lógico.

---


> **Meta:** dar ao editor de notas do momor (block editor gpui, `crates/notebook/src/note_editor.rs`) as principais capacidades do editor do ZenNotes — **interpretação de equações (LaTeX/math)** e **criação de gráficos/diagramas** (mermaid, function-plot, etc.) — **funcionando dentro do nosso sistema de blocos**, que já são arquivos markdown estilo Notion.
>
> **Decisão de base (travada):** NÃO trocamos o modelo. Continuam blocos discretos (cada bloco um `Editor` gpui) e persistência markdown round-trip. As features do ZenNotes entram como **novos tipos de bloco renderizados** + camadas por cima. Referência: [zennotes-editor-deep-dive.md](zennotes-editor-deep-dive.md), [zennotes-notes-core-analysis.md](zennotes-notes-core-analysis.md).

---

## 0. A realidade técnica (por que não é copy-paste)

O editor do ZenNotes é **web**: CodeMirror 6 + KaTeX + Mermaid + libs JS que renderizam para **DOM/SVG**. O momor é **nativo** (gpui/Rust, sem DOM, sem JS). Então:

- ❌ Não dá pra portar o código (TS→Rust, DOM→gpui).
- ✅ Dá pra portar o **resultado**: toda feature "rica" do ZenNotes produz, no fim, um **SVG**. E o gpui **renderiza imagens**. Então o denominador comum é: **fonte → SVG → bitmap → bloco**.

### O primitivo central: "bloco renderizado"

```
fonte (LaTeX / mermaid / plot spec)   ← fica no markdown, num fenced block
        │  render (nativo Rust, ou sidecar)
        ▼
      SVG string
        │  resvg (usvg + tiny-skia, 100% Rust)
        ▼
   RGBA bitmap  ──cache por hash(fonte)──►  gpui `img()` no bloco
```

- **Fonte no markdown**: um bloco math/diagrama é só um **fenced code block** com linguagem especial (` ```math `, ` ```mermaid `, ` ```plot `). O momor **já** faz parse de ` ```lang ` (a linguagem vai em `Block.lang`/`data`) e round-trip. **Zero mudança de schema** — o SVG é artefato de render, cacheado, nunca salvo na nota.
- **Render**: nativo onde dá (math, plots); sidecar (subprocess) só onde é inevitável (mermaid/tikz). Se o render falha/indisponível → o bloco degrada pra mostrar a fonte como code block. (ponytail: degradação graciosa, nunca quebra a nota.)
- **Cache**: `HashMap<u64 hash da fonte, Bitmap>` em memória + opcional em disco (`~/.../momor/render-cache/<hash>.png`). Re-render só quando a fonte muda.

Esse primitivo serve **math, mermaid, function-plot, tikz e até excalidraw-como-imagem** — todos convergem pra "SVG → bitmap → bloco".

---

## 1. Onde isso encaixa no momor (arquitetura atual)

| Peça atual | Arquivo | O que muda |
|---|---|---|
| `enum BlockKind` (Paragraph, H1-3, Bullet, Numbered, Todo, Quote, **Code**, Divider, **Image**) | `note_editor.rs` | + `Math`, `Diagram` (ou reusar `Code` com lang especial — ver §2) |
| `parse_blocks` (já entende ` ```lang `, indent, `![](path "w")`) | `note_editor.rs` | Reconhecer langs renderizáveis → marcar o bloco como renderizado |
| `render_block` | `note_editor.rs` | Se bloco renderizável e cache tem bitmap → mostra `img`; senão mostra a fonte (editável) |
| `apply_code_language` (carrega highlight async) | `note_editor.rs` | Análogo: `render_rich_block` (async) que gera o bitmap e faz `cx.notify()` |
| Persistência markdown | `store.rs` | Nenhuma — a fonte já é fenced code |

**Chave:** reusar o padrão que já existe pro highlight de código (`apply_code_language` roda async e atualiza o bloco). O render de math/diagrama é a mesma coisa: async → SVG → bitmap → `cx.notify()`.

---

## 2. Modelagem no bloco (decisão)

Duas opções; recomendo a **A** (mais lazy, round-trip grátis):

- **A (recomendada) — reusar `BlockKind::Code` com langs renderizáveis.** Langs `math`, `mermaid`, `plot`, `tikz` viram "renderizáveis". O bloco guarda a fonte no editor (como qualquer code block) e ganha um campo `rendered: Option<RenderedSvg>`. Quando focado → edita a fonte; quando desfocado e há bitmap → mostra a imagem (toggle estilo ZenNotes: clique/entra edita, sai renderiza). Round-trip automático (já é ` ```mermaid ... ``` `).
- **B — novos `BlockKind::Math` / `BlockKind::Diagram`.** Mais explícito, mas exige estender parse/serialize e o `/`-menu. Só vale se quisermos UX bem diferente de code block.

> Vou assumir **A** no resto do plano. Marca `ponytail:` no código explicando que math/diagram = code block renderizável.

Estrutura de render (nova, em `note_editor.rs` ou `crates/notebook/src/render_block.rs` novo):
```rust
struct RenderedSvg { hash: u64, image: Arc<gpui::RenderImage>, w: f32, h: f32 }
enum RichKind { Math, Mermaid, Plot, Tikz }   // derivado da lang
fn rich_kind(lang: &str) -> Option<RichKind>
async fn render_rich(kind: RichKind, source: &str) -> Result<String /* svg */>
fn svg_to_image(svg: &str) -> Result<RenderImage>   // resvg
```

---

## 3. Roadmap por fases (prioridade = valor ÷ esforço, math e gráficos primeiro)

### Fase 0 — Fundação de render (pré-requisito de tudo) · risco baixo
Entrega o primitivo "SVG → bitmap → bloco" sem nenhum renderer ainda.
- Deps: `resvg` + `usvg` + `tiny-skia` (100% Rust, sem C/JS). Add no `crates/notebook/Cargo.toml`.
- `svg_to_image(svg) -> RenderImage` (resvg rasteriza; escala por DPI da janela).
- Cache `RenderCache { map: HashMap<u64, RenderedSvg> }` como global gpui ou campo do `NoteDoc`.
- `render_block`: se `Block` é code-renderizável e cache tem bitmap → `img(bitmap).max_w(...)`; toggle editar/renderizar (reusa a lógica de focus que já existe).
- **Teste**: um SVG hardcoded (`<svg><rect/></svg>`) rasteriza e aparece num bloco. `demo()`/test com assert de dimensões > 0.

### Fase 1 — **Equações / Math (LaTeX)** · a prioridade nº 1 do usuário · risco médio
Bloco ` ```math ` (e `$$...$$`) renderiza a equação.
- **Renderer nativo primeiro**: crate **`rex`** (ReX — typesetter de math LaTeX → SVG, puro Rust). Se a cobertura de símbolos bastar, é offline e sem sidecar.
- **Fallback** (se `rex` faltar símbolo): render sidecar KaTeX→SVG (§6).
- `parse_blocks`: reconhecer ` ```math ` e linhas `$$...$$` como bloco math.
- Async: ao criar/editar o bloco math, `render_rich(Math, src)` → SVG → cache → `cx.notify()`.
- **ponytail cut:** só **math em bloco** (`$$`/```math). **Inline `$x^2$` dentro de parágrafo fica pra depois** (parágrafo é um `Editor` único; inline SVG no meio do texto é bem mais difícil no modelo de blocos). Marca e segue.
- **Teste**: `$$E=mc^2$$` gera bitmap; equação inválida degrada pra code block sem crashar.

### Fase 2 — **Gráficos de função (function-plot)** · "criar gráficos" nativo e barato · risco baixo
Bloco ` ```plot ` com `y = sin(x)` (ou spec) desenha o gráfico.
- **100% nativo**: avaliar a expressão (crate `meval` ou `evalexpr`) numa malha de x, gerar o **SVG do path** à mão (eixos + curva). Sem sidecar, sem deps pesadas.
- Suporta o essencial do function-plot do ZenNotes (uma ou N funções, range configurável).
- **Teste**: `y=x^2` gera path com N pontos; range inválido → mensagem no bloco.

### Fase 3 — **Mermaid (fluxogramas/diagramas)** · o "criar diagramas" mais pedido · risco alto
Bloco ` ```mermaid ` renderiza o diagrama. **Aqui não tem jeito nativo** (não existe port Rust de mermaid).
- **Render sidecar** (§6): um processo headless (Node + `mermaid`, empacotado, ou binário `mmdc`) recebe a fonte e devolve SVG. É exatamente o que o ZenNotes faz pro TikZ (`renderTikz` no main process).
- Cache agressivo por hash (mermaid é caro).
- **ponytail cut / degradação:** se o sidecar não estiver instalado, o bloco mostra a fonte mermaid como code block + um botão "Renderizar (requer sidecar)". App nunca depende do sidecar pra abrir.
- **Teste**: com sidecar mockado devolvendo SVG fixo, o bloco vira imagem; sem sidecar, vira code block.

### Fase 4 — TikZ / Excalidraw · opcional · risco alto
- **TikZ**: precisa de toolchain LaTeX (como o ZenNotes). Mesmo sidecar, mas dependência pesada. **Defer** salvo pedido explícito.
- **Excalidraw**: app de desenho inteiro (canvas JS). No momor: **embutir como imagem** (importar `.excalidraw`/PNG) em vez de editar. Editar nativo = projeto à parte. **Defer.**

### Fase 5 — Features de conhecimento (as "outras funções deles") · incremental
Depois do rich-content, portar o resto do poder do ZenNotes pros blocos (cada um é um sub-plano curto):
1. **Wikilinks `[[nota]]` + autocomplete** — completion no editor de bloco (o momor já tem CompletionProvider do `/`-menu; adicionar source `[[` que lista notas do store). Navegação ao clicar.
2. **Backlinks** — inverter os wikilinks de todas as notas (o store já tem `all_notes`); painel "Referências a esta nota".
3. **Outline** — extrair headings dos blocos H1-3 → painel/menu de salto.
4. **Tasks agregadas** — varrer blocos `Todo` de todas as notas → uma view de tarefas (o momor já tem `Todo(bool)`).
5. **Modo preview / split** — um toggle que renderiza a nota inteira read-only (útil pra ver tudo renderizado). Menos crítico já que blocos já são "WYSIWYG".
6. **Toolbar de seleção** (bold/italic/link/highlight) — flutuante ao selecionar texto num bloco.
7. **Tabelas** como bloco dedicado (o momor marcou tabela como "próxima feature grande" no note_editor.rs).

> Ordem sugerida da Fase 5: wikilinks+backlinks (maior valor p/ um app de notas conectadas) → outline → tasks → resto.

---

## 4. Sequência de entrega recomendada (o caminho lazy-but-right)

```
Fase 0 (fundação SVG→img)  ─┐
Fase 1 (math)              ─┤  ← núcleo pedido: equações
Fase 2 (function-plot)     ─┘  ← núcleo pedido: gráficos (nativo, barato)
        ── ponto de valor entregável: notas com equações e gráficos ──
Fase 3 (mermaid, com sidecar opcional)   ← diagramas ricos
Fase 5.1-5.2 (wikilinks + backlinks)     ← vira "second brain"
Fase 5.3+ (outline, tasks, toolbar, tabelas)
Fase 4 (tikz/excalidraw)   ← só se pedirem
```

Entregar **0+1+2 juntos** já dá o "editor do ZenNotes no momor" no que o usuário priorizou (equações + gráficos), sem sidecar, tudo nativo. Mermaid (Fase 3) é o primeiro que exige o sidecar.

---

## 5. Dependências novas (Cargo)

| Fase | Crates | Peso |
|---|---|---|
| 0 | `resvg`, `usvg`, `tiny-skia` | médio (puro Rust, sem C) |
| 1 | `rex` (math→SVG) *ou* sidecar KaTeX | leve nativo / médio se sidecar |
| 2 | `meval` ou `evalexpr` (avaliar expressão) | leve |
| 3 | — (sidecar externo, não é crate) | pesado operacionalmente |

Nenhuma exige rede. Tudo funciona offline (exceto o custo de empacotar o sidecar do mermaid).

---

## 6. O "render sidecar" (só Fases 3-4)

Padrão idêntico ao `renderTikz` do ZenNotes (que compila no processo main). No momor:
- Um binário/script headless (Node+mermaid empacotado, ou `mmdc`) que lê `{kind, source}` no stdin e escreve SVG no stdout.
- momor chama via `std::process::Command` (async, com timeout), cacheia o SVG por hash.
- **Opcional por design**: ausência do sidecar = degradação pra code block. Não vira dependência de boot.
- Empacotamento: incluir o sidecar nos resources do build (como já fazemos com ícones), ou detectar `mmdc` no PATH.

> Alternativa a avaliar: **Kroki** (servidor local em Docker que renderiza mermaid/plantuml/etc.). Mais simples de integrar (HTTP), mas exige Docker/rede — contra o espírito offline. Sidecar empacotado é o caminho preferido.

---

## 7. Riscos & cortes honestos (ponytail)

| Risco | Mitigação / corte |
|---|---|
| `rex` não cobrir todo LaTeX | Fallback sidecar KaTeX; ou limitar ao subset comum e avisar |
| Mermaid exige JS runtime | Sidecar **opcional** + degradação; nunca bloqueia o app |
| Inline math (`$x$` no meio do texto) | **Deferido** — só block math na v1 (modelo de blocos dificulta inline) |
| TikZ/Excalidraw | **Deferidos** (toolchain LaTeX / app de canvas) |
| resvg não bater 100% com o SVG do mermaid | Aceitável p/ v1; ajustar fontes/CSS depois |
| Performance (render em nota grande) | Cache por hash + render async + só renderiza bloco visível |

**O que este plano NÃO tenta ser:** um clone do CodeMirror. O momor fica com blocos; ganha o *conteúdo rico* (equações, gráficos, diagramas) e depois as *conexões* (wikilinks/backlinks/outline) do ZenNotes.

---

## 8. Definição de pronto (por fase)

- **Fase 0-2 (MVP):** criar um bloco ` ```math ` com `$$…$$` e um ` ```plot ` com `y=f(x)` numa nota, ver renderizado, salvar, reabrir e continuar renderizado (round-trip). Fonte editável ao focar. Sem crash em fonte inválida.
- **Fase 3:** ` ```mermaid ` renderiza com sidecar; sem sidecar, degrada.
- **Fase 5.1:** digitar `[[` sugere notas; clicar no link abre a nota; painel de backlinks lista quem aponta pra ela.

---

## 9. Próximo passo concreto

Se aprovar, começo pela **Fase 0 + Fase 1** (fundação SVG + math nativo com `rex`), que é o menor caminho até "equação renderizando num bloco do momor" — e valido com um ` ```math ` real antes de seguir pro function-plot e mermaid.
