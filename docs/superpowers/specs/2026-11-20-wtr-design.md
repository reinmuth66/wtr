# wtr 設計書

`wtr` は、ファイルの表示と Claude Code の作業履歴を 1 つの画面で見る TUI。状態ファイルを見張り、そこに書かれたファイルとセッションを表示する。

## 意図

- herdr の preview の枠に常駐させ、エージェントの作業を左で確かめ、右で指示を出す使い方を支える。herdr の mod の部品では、j/k のスクロール、本物の色付け、同じ画面で nvim を開くことが難しかったので、ターミナルの道具として作り直す
- herdr には依存しない。入力は状態ファイル 1 つで、herdr の中では dotfiles の mod がそれを書く。herdr の外でも、状態ファイルを書けば同じように使える
- Claude Code の会話には道具の行を出さない使い方をするので、道具の呼び出しの中身は `wtr` で見る。情報量は Claude Code の表示 (道具の行と `⎿` の要約、ctrl+o の全表示) と同等にする
- ブラウザーは端末で動く terminal-browser を preview の `stack` の別の枠に常駐させ、エージェントの操作 (playwright-mcp の代わり。別の Chrome の窓が立ち上がらない) と、人が HTML の見た目を確かめることを同じブラウザーで行う (dotfiles Issue #91)。`wtr` は、表示中の HTML や作業履歴の URL をそのブラウザーに開かせる

## 目標

段階 1 (最初の完成):
1. 状態ファイルを見張り、書かれたファイルを表示する。ファイルが書き換わったら出し直す
2. テキストとコード (Markdown を含む) を色付きで、行番号と折り返しつきで出す。JSON は整形してから出す。CSV と TSV は列を揃えた表で出す。画像を出す
3. j/k などでスクロールし、o / O で nvim や専用のツールを同じ画面で開く
4. タブごとに表示の位置を覚える

段階 2:
5. 状態ファイルに書かれたセッションの jsonl を読み、道具の呼び出しとユーザーの指示を 1 行ずつ並べる (作業履歴)。追記に追従する
6. 一覧と展開を切り替え、展開では道具ごとの中身 (Edit は delta の差分) を出す
7. ファイルの表示と作業履歴を Tab で切り替える
8. `O` で HTML のファイルと URL を terminal-browser に開かせる (下の「ブラウザーに開かせるとき」と `--on-browse`)。段階 1 の間、HTML の `O` は nvim で開く (2026-11-23 の判断)
9. 知らない記録の種類を数える `wtr scan-records`

## 目標にしないこと

- git graph。herdr の mod が受け持つ (同じ preview の枠を `stack` で切り替えて使う)
- gh-dash を開くこと。preview の `stack` は git graph (mod)、`wtr`、gh-dash、terminal-browser の 4 つを切り替える。gh-dash は `stack` の別の枠に常駐し、herdr のタブを切り替えると、そのタブの cwd で dotfiles が起動し直す (作業中のリポジトリに絞るため。2026-11-23 の判断)。`wtr` は関わらない。`stack` でどの枠を出していたかは、Herdr.app では herdr のタブごとではなく、レイアウトに 1 つだけ覚えられる (`layouts/.state/`。2026-11-23 に資料で確かめた)。タブごとに覚えるなら dotfiles の mod が受け持つ
- エージェント (assistant) の文章を出すこと。会話は herdr の agent-area で見る
- ファイルを選ぶ機能 (ファイルの一覧、検索)。選ぶのは状態ファイルを書く側 (herdr の tree など) の受け持ち
- PDF、Office、3MF、KiCad の縮小画像と、圧縮ファイルの中身の一覧。段階 1、2 では種類と大きさだけを出す
- Markdown の整形 (glow のような表示)。段階 1 ではテキストとして色付けする
- HTML の描画。HTML はコードとして出し、見た目は terminal-browser で見る
- herdr の枠の切り替え。ブラウザーに開かせたあとの切り替えは、設定で渡すコマンド (`--on-browse`) に任せる
- terminal-browser を起動すること。ブラウザーは外 (dotfiles) が常駐させる
- crates.io への公開

## 失敗とみなすこと

- 状態ファイルが変わってから表示が切り替わるまで、目に見えて待たされる
- 数 MB のファイルや jsonl で、画面が固まる
- nvim などから戻ったあと、画面が崩れたままになる
- 作業履歴の jsonl の最後の行が書きかけのとき、その呼び出しが抜け落ちる
- 日本語の幅がずれて、列や折り返しが崩れる

## 前提と制約

- Rust で書く。TUI は ratatui
- 動かす環境は macOS (aarch64)。herdr のターミナルは Kitty graphics を表示できる
- dotfiles (nix-darwin + home-manager) から、nix でこのリポジトリをビルドして入れる。nix でビルドできる形 (Cargo.lock を置く) にする
- 色は catppuccin-macchiato に揃える
- アイコンは Nerd Font のマークを使う。固定の文言 (行数などの単位、状態の名前) は英語かマークにし、日本語を使わない。中身として出る文字 (コマンド、指示の文、質問) はそのまま出す
- 同じ画面で開く外のツール: nvim、csvlens、fx。差分の表示に delta。ブラウザーに開かせるのに terminal-browser。どれも dotfiles 側が nix で入れる。見つからなければ、その旨を出して何もしない
- terminal-browser について 2026-11-23 に Herdr.app (herdr-core) で確かめたこと (v0.15.9):
  - 分割しない `terminal-browser open <url>` で、枠の中にページが画像で出て、操作できる
  - Herdr.app のターミナルは `TERM_PROGRAM=ghostty` を名乗るため、terminal-browser は Ghostty 本体を AppleScript で操作しようとし、`ls` や `action` が `Ghostty is not running` で失敗する。`TERM_PROGRAM` を外して呼べば通る。terminal-browser の herdr 連携 (`HERDR_PANE_ID`) は Herdr.app では効かない
  - 同じ端末タブの見分けが効かない (`inCurrentTab: false`) ので、`--browser <key>` で相手を指定する。key は `terminal-browser ls --all --json` で得る
  - `action -- open <url>` は terminal-browser が横取りし、必ず新しいタブを作る。開いた直後は、そのタブに `action` が届かない (読み込みが終わると届く)。`action -- goto <url>` は agent-browser にそのまま渡り、今のタブを開き直す (タブは増えない)
  - `file:///abs/path` の HTML も開ける
  - ページが切り替わるときに画面が点滅する

## 作り方の検討

| 案 | 内容 | 結果 |
|---|---|---|
| **Rust (ratatui)** | syntect で色付け (bat と同じ)、ratatui-image で画像 | **採る**。画像の扱いやすさと、bat と同じ色付けを重く見た |
| Go (bubbletea) | chroma で色付け、glamour で Markdown | 採らない。画像は Kitty graphics を自前で送る必要がある |
| herdr の mod のまま | 組み込みの Editor と TextView | 採らない。部品をコードからスクロールできず、色付けの結果も取り出せない |

テキストとコードの表示:

| 案 | 結果 |
|---|---|
| **syntect で自前に描く** | **採る**。スクロールと位置の記憶を `wtr` が完全に持てる |
| bat を外から呼んで出力を描く | 採らない。幅が変わるたびに呼び直すことになり、行番号と折り返しを持てない |

作業履歴の既存のツール (2026-11-23 に調べた):

| ツール | 内容 | 結果 |
|---|---|---|
| tail-claude (Go) | 会話を流れで出し、道具の呼び出しを展開する。エージェントの文章と思考も出す | 使わない。会話そのものを出す作りで、道具の行だけを並べる使い方と違う。herdr の状態ファイルにも追従しない |
| claude-esp / claude-esp-rs (Go / Rust、MIT) | 別の端末に思考、道具、サブエージェントを流す。記録の種類の対応が一番広い | 使わない。流れを出す作りで、1 行ずつの一覧と展開ではない |
| claude-code-trace (Rust + Tauri / Web / Textual) | 会話の閲覧、追従、効率の分析 | 使わない。バックエンドの常駐と HTTP を挟み、重い |

ブラウザーの扱い:

| 案 | 結果 |
|---|---|
| **枠を分ける** (terminal-browser を `stack` の別の枠に常駐させ、`wtr` は開かせるだけ) | **採る**。人とエージェントが同じブラウザーを見るので、エージェントのテストの画面もそのまま確かめられる |
| `wtr` の中で開く (nvim と同じく一時停止して terminal-browser を開く) | 採らない。閉じるとブラウザーも終わり、エージェントが使うブラウザーと別物になる |

## 決めたこと

### 起動と入力

- `wtr --state <path>`: 状態ファイルを見張る (herdr の中の使い方)
- `--on-browse <cmd>`: ブラウザーに開かせたあとに実行するコマンド (どの起動の形にも付けられる)
- `wtr <file>`: そのファイルを表示する。`wtr --session <jsonl>`: その作業履歴を表示する (herdr の外の使い方。状態ファイルなしで動く)
- 状態ファイルは JSON。

```json
{ "tab": "<タブを表す文字>", "file": "/abs/path", "session": "/abs/path/to/session.jsonl" }
```

- `tab`: この値ごとに表示の状態 (ファイルの表示のスクロール位置、作業履歴の選んだ行と展開、どちらの表示を出しているか) を覚える。ファイルの表示のスクロール位置は、タブの中でファイルごとに覚え、同じタブで前に見たファイルに戻ったら前の位置から出す。同じタブで `session` が変わったら、選んだ行と展開を捨て、一番下から始める (2026-11-23 の判断)。値が変わったら、その値の状態に切り替える。初めての値では、作業履歴の表示 (一番下) から始める (herdr の中では、初めてのタブは preview の枠ごと隠れ、出したときにそのエージェントの作業を見ることが多いため。設計者の判断。枠を隠すのは dotfiles Issue #129)。無ければ 1 つの状態だけを持つ
- `file`: ファイルの表示に出すもの。フォルダなら、中に README (`README.md`、`readme.md`、`README.markdown`、`README` の順、大文字と小文字を区別しない) があればそれを出し、なければ表示を変えない
- `session`: 作業履歴に出す jsonl
- どれかが無い、読めない、壊れているときは、その表示に「なし」を表すマークと短い英語 (`no file`、`no session`) を出す。状態ファイルが壊れている (書きかけ) ときは、直前の内容のまま待つ
- 状態ファイルを書くのは外 (herdr の中では dotfiles の mod)。`wtr` は読むだけ

### ファイルの表示 (段階 1)

| 種類 | 表示 |
|---|---|
| テキスト、コード、Markdown | syntect で色付け。言語の定義は bat と同じ集まり (`two-face` などで、nix などを含む) を使う。テーマは catppuccin-macchiato (two-face に `CatppuccinMacchiato` として入っている。2026-11-23 に確かめた)。行番号を出し、行末で折り返す (日本語の幅を unicode-width で数える) |
| JSON | 整形してから、コードとして出す。読めなければ元のまま出す |
| CSV、TSV | 列を揃えた表。引用符の中のカンマと改行を正しく扱う。列の数が違う行は、足りないセルを空にする。大きいファイルは先頭の 300 行だけを出し、切ったことを示す |
| 画像 (png、jpg、gif、webp、svg、bmp) | ratatui-image で出す。表示できない形式は、種類と大きさの表示に回す。herdr のターミナルは Kitty graphics を表示するが、問い合わせへの返事が読み終わりの合図より遅れて届き、入力に漏れる。そのため `TERM_PROGRAM=ghostty` のときは Kitty の問い合わせを送らずに Kitty で出す (yazi と同じ判定)。セルの大きさは隠れている間は仮の値になるので、画面の大きさが変わったときに聞き直す (2026-11-20 に確かめた) |
| それ以外 (PDF、Office、3MF、KiCad、圧縮ファイル、NUL を含むファイルなど) | 種類 (拡張子) と大きさだけ |

- 種類の分け方は yazi の標準に揃える。yazi は `file -bL --mime-type` の MIME で分ける (`text/*` はコード、`application/json` と `application/ndjson` は JSON、`image/*` は画像、それ以外は種類と大きさ)。これに従い、UTF-8 でないテキスト (Shift_JIS、Latin-1) と BOM 付き UTF-16 はテキストとして出し (UTF-16 は文字に直す)、拡張子の無い JSON は JSON として出す。上の表が名指しする CSV、TSV、KiCad は、`file(1)` の判定より上の表を優先する。YAML はコードとして出す。jsonl は JSON として色付けする (2026-11-20 に yazi の標準の設定と `file(1)` の結果を確かめた)
- 大きなファイル: 色付けは画面に見える範囲まで先に行い、残りは後回しにする。一定の大きさ (作る人が決める。目安 数 MB) を超えたら色なしで出す
- 見出しの段 (種類のマークとファイル名) は出さない。wtr はプレビューを出すことだけを受け持つ。知らせ (`fx not found` など) は、あるときだけ 1 行目の右端に、文字の幅だけ重ねて出す (左の中身は隠さない)。次のキーか、3 秒経ったら消す。tree の仕様によっては見出しを戻すので、コードはコメントアウトで残す (Issue #1、2026-11-24 の判断)
- ファイルが書き換わったら出し直す。スクロールの位置はできるだけ保つ

### 作業履歴 (段階 2)

#### 読み方

- jsonl の形は公式には定義されていない。Claude Code の資料 (`sessions.md` の「Where transcripts are stored」) は「形は内部のもので版ごとに変わり、直接読むスクリプトはどの版でも壊れうる」としている。実際に新しい種類の記録が版ごとに増えている (下の「形の変化の追い方」)
- そのため、知っている形の記録だけを拾い、知らない `type`、`subtype`、`attachment.type` と、読めない欄は黙って捨てる。捨てても落ちない、行が崩れない、を守る。知らない種類を見る手段として、捨てた種類を数えて出す起動の印 (`--debug-unknown` など。名前は作る人に任せる) を持つ
- 確かめた版: Claude Code 2.1.311〜2.1.326 の記録 (2026-11-23)。試験の材料は実際の記録から作り、版が上がって壊れたら材料を足して直す
- 記録の種類の表 (`type`、`subtype`、`attachment.type`、`message.content[].type` の組)。1 か所にまとめ、「使う」と「捨てる」のどちらかに必ず入れる。どちらにも無いものが「知らない種類」になる。初めの中身 (2026-11-23 に手元の 2.1.296〜2.1.326 の記録にあった種類から作った):
  - 使う: `assistant` (中身の `tool_use`。`text`、`thinking` は捨てる)、`user` (中身の `text`、`image`、`tool_result`)、`system:compact_boundary`、`system:local_command`、`attachment:queued_command`、`attachment:hook_success`、`attachment:hook_non_blocking_error`、`attachment:hook_blocking_error` (手元の記録には無いが、Claude Code の hook の仕組みにある)、`attachment:hook_cancelled` (hook の出力と同じ場所に `─ hook · cancelled ─` として出す)、`attachment:diagnostics` (編集後の LSP の指摘)、`system:turn_duration` (作業時間)
  - 捨てる (`type` だけで決まるもの): `api-request`、`api-request-blob`、`api-request-shape`、`last-prompt`、`atis-latch`、`mode`、`ai-title`、`permission-mode`、`session-owner`、`file-history-snapshot`、`file-history-delta`、`queue-operation`、`pr-link`、`frame-link`、`agent-name`、`tools-used`、`artifact-autoreact-ledger`、`artifact-comment-monitor`、`cost-state`
  - 捨てる (`system`): `away_summary` (recap)、`informational` (使用量の上限などのお知らせ)、`stop_hook_summary`。`turn_duration` は使う (指示の行の作業時間)
  - 捨てる (`attachment`): `agent_listing_delta`、`async_hook_response`、`at_mention_reference`、`auto_mode`、`command_permissions`、`compact_file_reference`、`credential_org`、`critical_system_reminder`、`date`、`deferred_tools_delta`、`deferred_tools_record`、`directory`、`edited_text_file`、`environment`、`file`、`hook_additional_context`、`hook_system_message`、`instructions`、`invoked_skills`、`mcp_instructions_delta`、`model`、`nested_memory`、`plan_mode`、`plan_mode_exit`、`prompt_snapshot`、`read_truncation_notice`、`remote_session_change`、`session_context`、`silent_turn_reminder`、`skill_listing`、`task_reminder`、`task_status`、`thinking_drop`、`total_tokens_reminder`
- 「捨てる」は「Claude Code の画面に出ない」という意味ではない。`away_summary` (離れていた間のまとめ、recap)、`informational` (お知らせ)、`plan_mode` などは Claude Code の画面に出るが、`wtr` の 3 つの行 (道具の呼び出し、指示、要約の区切り) に当たらないので捨てる
- 知らない種類も画面には出さない (数えるだけ)
- 知らない種類を Issue にする仕組み (段階 2 で作る):
  - `wtr scan-records` (名前は作る人に任せる): `~/.claude/projects` の jsonl を読み、知らない種類ごとに、件数、初めて現れた `version`、出てきた欄の名前 (キーだけ) を JSON で標準出力に出す。既に知っている種類で、使っている欄 (`toolUseResult.structuredPatch`、`compactMetadata.preTokens` など) が無くなったことも出す。`wtr` は結果を出すだけで、gh は呼ばない (設計者の判断。gh の有無や認証、重複の確かめを `wtr` に持たせないため)。出力の JSON の形は dotfiles との約束として設計書か README に書く
  - dotfiles の nix で launchd の仕事を作り、1 日 1 回 `wtr scan-records` を動かす。dotfiles のスクリプトが結果を読み、知らない種類があれば `gh issue create` で `reinmuth66/wtr` (private) に出す。題名に種類の名前を入れ、同じ題名で開いている Issue があれば出さない
  - Issue の本文には種類の名前、版、欄の名前だけを書き、記録の中身 (指示の文、コマンド、ファイルの中身) は書かない。GitHub に会話の中身を出さないため
  - TUI からは Issue を出さない (常駐の画面から勝手にネットへ書かないため)
- 形の変化の追い方: 公式の CHANGELOG は記録の形の変化をほとんど書かない (見た目や保存の不具合の直しが中心)。各行に `version` が付いているので、手元の記録で `type` / `subtype` / `attachment.type` が初めて現れた版を数えれば、増えた種類が分かる (2026-11-23 に 2.1.296〜2.1.326 で 49 種類を数えた。手元の記録は約 1 か月分しか残らない)。他のツールの対応の履歴は二次情報なので頼らず、手元の記録を一次の情報にする
- jsonl の追記された分だけを読む。最後の行が書きかけなら、次の読み込みで完全な行として拾う。読む位置はバイトで持ち、完全な行の分だけ進める
- `isSidechain` が真の記録は飛ばす (子のエージェントの中の呼び出しは出さない。親の Agent の 1 行だけを出す)
- assistant の記録の `tool_use` で道具の行を作り、user の記録の `tool_result` で同じ id の行に結果を付ける。結果が先に来ても結べるようにする
- `toolUseResult` (物のときだけ) から、Edit の `structuredPatch` や、件数などの補足を取る
- ユーザーの指示の行: ユーザーが打った文とスラッシュコマンドを、指示の行にする。道具の結果、system-reminder、ターミナルで実行したコマンドの記録、背景のエージェントの完了通知 (`task-notification`)、サブエージェントの報告 (下の `agent-message`)、compact の要約 (`isCompactSummary` が真の user の記録) など、ユーザーが打っていないものは指示の行にしない。画像の添付は `[image]` と出す
- エージェントの作業中にユーザーが打った指示は、user の記録にならず、`type` が `attachment` で `attachment.type` が `queued_command`、`attachment.commandMode` が `prompt` の記録にだけ残る (文は `attachment.prompt`)。これも指示の行にする。`commandMode` が `task-notification` のもの (背景のエージェントの完了通知) と、`commandMode` が無いもの (仕組みが入れた文) は指示の行にしない (2026-11-23 に実際の jsonl で確かめた)
- スラッシュコマンドは 2 か所に残る。モデルに送るもの (`/effort`、`/clear`、skill など) は user の記録の中の `<command-name>`、`<command-args>` に、手元だけで動くもの (`/restart`、`/btw` など) は `type` が `system` で `subtype` が `local_command` の記録の `content` に、同じ形の印で残る。どちらも指示の行にする (2026-11-23 に実際の jsonl で確かめた)
- `[Request interrupted by user]` と `[Request interrupted by user for tool use]` の文は、指示の行にしない (中断の印。下の状態の決め方に使う)
- 状態の決め方 (2026-11-23 に実際の jsonl で確かめた):
  - 失敗: `tool_result` の `is_error` が真
  - 拒否: `tool_result` の `is_error` が真で、中身が `The user doesn't want to proceed with this tool use.` で始まる (`toolUseResult` は文字列 `User rejected tool use`)
  - 中断: 拒否と同じ形の結果のあとに、`[Request interrupted by user for tool use]` の文の記録が続く。道具の途中で中断すると、待っていた道具には拒否と同じ結果が付くため、続く文で見分ける
  - 実行中: まだ `tool_result` が無い。背景で動かした Agent は、`toolUseResult.status` が `async_launched` で、まだ完了通知が来ていない間 (下の「背景のエージェント」)
- hook の出力は、`type` が `attachment` で `attachment.type` が `hook_success`、`hook_non_blocking_error`、`hook_blocking_error` の記録にある (失敗の 2 つは小見出しを red にする) (`hookEvent`、`hookName`、`stdout`、`stderr`、`exitCode`、`toolUseID`)。`toolUseID` で道具の行に結ぶ。`PostToolUse` 以外 (`SessionStart`、`Stop`) は道具に結ばないので出さない
- `isSidechain` が真の記録は、親の jsonl には無く、`subagents/` の下の別のファイルにある。飛ばす決まりは念のための守りとして残す
- 行の種類は「道具の呼び出し」(tool_use の id が単位)、「指示」、「要約の区切り」の 3 つだけにする。サブエージェントの報告と背景のエージェントの完了通知は新しい行にせず、元の Agent の行に結ぶ (設計者の判断)
- 背景のエージェント (2026-11-23 に実際の jsonl で確かめた):
  - Agent を背景で動かすと、`tool_result` は起動した直後に返る。中身は `Async agent launched successfully...` で、`toolUseResult` は `status: "async_launched"` と `agentId` を持ち、道具の回数、トークン数、時間は持たない。この `tool_result` では成功にせず、実行中のままにする
  - 終わると、`type` が `attachment` で `attachment.type` が `queued_command`、`commandMode` が `task-notification` の記録が来る。`attachment.prompt` の `<task-notification>` の中の `<tool-use-id>` で元の Agent の行に結び、`<status>` で状態を決める (`completed` は成功、それ以外は失敗)。`<usage>` の `subagent_tokens`、`tool_uses`、`duration_ms` で補足を作る。同じエージェントが続けて動くと、通知は何度も来る (最後のものを使う)。通知の `<summary>` が「自分の背景の仕事を待っている」ことを示す途中の通知でも、状態は `<status>` のとおりにする
  - サブエージェントの報告は、親の jsonl では user の記録として届く。文は `Another Claude session sent a message:` で始まり、`<agent-message from="<agentId>">` と `[Subagent hand-back]` の印のあとに報告の全文が続く。`from` の値と Agent の `toolUseResult.agentId` で元の Agent の行に結び、展開の `─ result ─` に出す。報告が無ければ `─ result ─` には `tool_result` の中身を出す
  - サブエージェントの中の `SubagentHandback` の呼び出しは、`subagents/agent-<agentId>.jsonl` にだけあり、親の jsonl には無い
  - `[Subagent hand-back]` の印の無い `agent-message` (ほかのセッションからの知らせ) は、どの行にもしない
  - 背景で動かさない Agent は、今までどおり `tool_result` と `toolUseResult` から結果と補足を取る
- 要約の区切りの行: `type` が `system` で `subtype` が `compact_boundary` の記録で作る。`compactMetadata` から、手動か自動か (`trigger`) と前後のトークン数 (`preTokens`、`postTokens`) を取る。直後の `isCompactSummary` が真の user の記録の中身 (要約の全文) を、この行の中身にする
- セッション (`session` の値) が変わったら、読み直す

#### 一覧

並びは上が古く、下が新しい。開いたときは一番下にいる。一番下にいる間だけ、新しい行に追従して自動でスクロールする。上を読んでいる間と、展開している間は動かさない。

1 行の形:

```
 <状態> <種類> <要点>                              <補足>
   󰀄 ユーザーの指示の 1 行目 (blue)
 󰄬 󱆃 nix flake check                                 󰉻 12
 󰄬 󰏫 preview/logic.js                               +12 -3
```

- 1 列目は状態のマーク: 成功 `nf-md-check`、失敗 `nf-md-close`、拒否 `nf-md-shield_off`、中断 `nf-md-pause`、実行中 `nf-md-timer_sand`。指示の行は空ける。状態のマークには意味の色を付ける (成功 green、失敗 red など)
- 2 列目は種類のマーク: 指示 `nf-md-account`、Bash `nf-md-console`、Read `nf-md-eye`、Edit `nf-md-pencil`、Write `nf-md-file_plus`、Grep `nf-md-magnify`、Glob `nf-md-file_find`、WebSearch と WebFetch `nf-md-web`、AskUserQuestion `nf-md-help_circle`、Agent `nf-md-robot`、Skill `nf-md-book_open`、MCP の道具 `nf-md-puzzle`、それ以外は共通のマーク。マークは差し替えやすいよう 1 か所の表にまとめる
- 要点 (道具ごと): Bash はコマンドの 1 行目、Read / Edit / Write はパス、Grep はパターンとパス、Glob はパターン、WebSearch は検索語、WebFetch は URL、AskUserQuestion は最初の質問の見出し、Agent は説明、Skill は skill の名前、ToolSearch は検索語、MCP の道具は `<サーバー> <道具>` と主な引数、SendMessage / TaskStop は相手と要約、それ以外は道具の名前
- パスは、セッションの cwd からの相対パスにする。cwd の外は `~` で短くする
- 補足 (右端に揃える): Bash は出力の行数 (`󰉻 12`)、Read は読んだ行数、Edit は `+12 -3`、Write は新規のマークと行数、Grep / Glob / ToolSearch は件数、WebSearch は回数と時間、WebFetch は HTTP の状態、AskUserQuestion は最初の質問で選ばれた答え、Artifact は URL、Agent は道具の使用回数とトークン数と時間 (`󱁤 36 · 113k · 3m 5s`)、失敗はエラーの 1 行目、拒否は `(rejected)`、中断は `(interrupted)`、実行中は `(running)` (どの道具でも。背景のエージェントも含む)。Skill、MCP の道具などは空
- 編集後の LSP の指摘: `type` が `attachment` で `attachment.type` が `diagnostics` の記録 (`files[].uri`、`files[].diagnostics[]` の `severity`、`message`、`range`) を、直前の同じパスの Edit / Write の行に結ぶ (記録に道具の id が無いため、パスで結ぶ。同じパスの行が無ければ出さない)。指摘があれば、その行の補足の先頭 (`+12 -3` や `new 40` の前) に件数を出す。Error を含めば `nf-md-close_circle` を red、Warning だけなら `nf-md-alert` を yellow で出す (例: `󰅚 2 +4 -1`。件数と行数の間は `+4` と `-1` の間と同じ空白 1 つで、`·` は入れない)。状態のマークは成功のまま変えない。展開では `─ diagnostics ─` の下に、指摘ごとに重さ、行番号、文を出す (2026-11-23 の判断)。`diagnostics` は「使う」に入れる
- マークの残り (2026-11-23 の仮の決定。1 か所の表で差し替える):
  - 種類: それ以外の共通 `nf-md-tools`、ToolSearch `nf-md-toolbox`、SendMessage `nf-md-message_arrow_right`、TaskStop `nf-md-stop_circle`、Artifact `nf-md-application`
  - 補足: 行数 (Bash、Read、Write) `nf-md-format_list_numbered`、Write の新規 `nf-md-new_box` (例: `󰎔 40`)、件数 (Grep、Glob、ToolSearch) `nf-md-pound`、時間 (作業時間、Agent の時間) `nf-md-timer_outline`、Agent の道具の回数 `nf-md-tools`。WebFetch の HTTP の状態はマークなし (`200`)
  - 状態の色: 成功 green、失敗 red、拒否 yellow、中断 peach、実行中 sky
  - 「なし」: `nf-md-file_hidden` と `no file`、`nf-md-history` と `no session`
  - 要約の区切り `nf-md-arrow_collapse_vertical`
- 揃え方: 補足は右端に揃える。要点と補足がぶつかるときは要点の末尾を `…` で切る。補足が長すぎるときは、補足も最大の幅 (画面の幅の 4 割) で切る。幅は unicode-width で数え、Nerd Font のマークの幅 (1 か 2) は起動の印 `--icon-width <1|2>` で変えられるようにする (既定は 1。設定ファイルは持たない。設計者の判断)。herdr のターミナルでマークが 1 マスに数えられることを 2026-11-23 に確かめた。ただし絵が右のマスにはみ出すことがあるので、マークのあとには必ず半角の空白を 1 つ置いてから次の文字を出す (マークどうしが並ぶときも同じ)
- 指示の行: 文の 1 行目を blue で出す。スラッシュコマンドはコマンド名と引数を出す。補足 (右端) に、その指示から始まったやりとりの作業時間 (`󱎫 33s`) を出す。作業時間は、そのやりとりの終わりに来る `type` が `system` で `subtype` が `turn_duration` の記録の `durationMs` から取る。まだ記録が無い (作業中) ときは空にする (2026-11-23 の判断)
- 会話の要約 (compact) の位置には、区切りの行を入れる。線の中に手動か自動か (`manual` / `auto`)、右端に前後のトークン数を出す

```
 ── 󰁯 compacted · manual ──────────── 630k → 10k
```

- 区切りの行も `j` / `k` で選べ、展開できる

#### 展開

```
 <前の履歴の 1 行>
──────────────────────────────────────
 <見出し: 状態、種類、名前、補足>
 <中身>
──────────────────────────────────────
 <次の履歴の 1 行>
```

- 上の端に前の履歴の 1 行、下の端に次の履歴の 1 行を、一覧と同じ形で残す。先頭なら上の端は無く、末尾なら下の端は無い。中身が短く隙間ができるときは、その分だけ後ろの履歴の行を増やして詰める
- 見出しは要点を繰り返さない。道具の行は「状態、種類のマーク、道具の名前、補足」、指示の行は「種類のマーク、`user` (スラッシュコマンドならコマンド名)、この指示のあとに使った道具の数 (成功と失敗)、作業時間」
- 中身は字下げする。見出しの名前の列 (状態と種類の 2 つのマークの右) から始め、折り返した行も同じ位置に揃える

```
 󰄬 󱆃 Bash                                   󰉻 12
     󰅂 nix flake check
       全体の検査を走らせる
     ─ output ─
     all checks passed
```

- 中身の小見出し (`output`、`input`、`result` など) は薄い色の英語
- 道具ごとの中身:

| 道具 | 中身 |
|---|---|
| 指示 | 文の全文 |
| 要約の区切り | 要約の全文 (`isCompactSummary` の記録の中身)。見出しは `󰁯 compacted`、手動か自動か、前後のトークン数 |
| Bash | `󰅂 <コマンド全文>` (頭の記号は `nf-md-chevron_right`)、説明 (あれば薄い色)、`─ output ─` と出力の全文。失敗なら小見出しを red にし、終了コードを添える (`─ output · exit 1 ─`) |
| Read | 読んだ範囲 (`L1-549`)、中身を syntect で色付け |
| Edit | パスと、delta の差分 (下) |
| Write | `new file` か `overwrite`、書いた中身を syntect で色付け |
| Grep / Glob | 条件 (`pattern:`、`path:`、`glob:`)、`─ results ─` と結果 |
| AskUserQuestion | 質問ごとに見出しと、選ばれた答え |
| Agent | 種類とモデル、`─ prompt ─` と指示の全文、`─ result ─` と報告の全文 |
| 拒否 | 入力 (Bash ならコマンド) と `─ result ─` |
| それ以外 (MCP を含む) | `─ input ─` と入力 (JSON を整形して色付け)、`─ output ─` と出力 |

- hook の出力が記録にあれば、中身の下に `─ hook · <種類> ─` として出す
- Edit の差分: `structuredPatch` から unified diff を組み立て、delta に渡して出力を描く。上下に並べ (unified。左右に並べる side-by-side にはしない)、行番号を出す。delta が無い、または `structuredPatch` が無い・空のときは、入力と出力をそのまま出す
  - 組み立てる diff には `--- a/<ファイル名>` と `+++ b/<ファイル名>` の見出しを付ける。delta は見出しのパスで言語を決め、見出しが無いと色が付かない。見出しは下の omit で画面から消すので、パスはファイル名だけでよい
  - delta の引数は呼び出し側で固定し、ユーザーの gitconfig に頼らない (dotfiles の delta は `side-by-side = true` なので `--no-gitconfig` で読まない。設計者の判断)。渡すもの:
    - `--no-gitconfig --line-numbers --syntax-theme 'Catppuccin Macchiato'`
    - `--file-style omit --hunk-header-style omit`: パスの見出し (`a/x ⟶ b/x`) と区切りごとの枠の見出しを消す。展開の見出しとパスが重なるため。区切りの間には空行が残り、それを区切りとして使う。出力の先頭に残る空行は捨てる
    - `--line-numbers-left-format '{nm:>N} ' --line-numbers-right-format '{np:>N} │'`: 行番号の欄の幅を固定する。N は `structuredPatch` の最大の行番号の桁数で、欄の幅は 2N+3 マスになる。既定の書式 (`{nm:^4}⋮`) は 5 桁以上の行番号で欄が広がり、行ごとに幅が揃わない
    - 差分の色: `--minus-style`、`--minus-emph-style`、`--plus-style`、`--plus-emph-style`、`--line-numbers-minus-style`、`--line-numbers-plus-style`、`--line-numbers-zero-style` に catppuccin-macchiato の色を渡す。既定の色 (暗い赤と緑の背景、256 色の行番号) は catppuccin と合わない
    - `--width` は渡さない。上下に並べる表示では折り返しに効かず、見出しを消したあとの出力は幅によって変わらない
  - delta の出力 (ANSI の色) は ansi-to-tui で ratatui の文字に直す
  - 折り返しは `wtr` が行う。先頭の 2N+3 マス (行番号の欄) の右側を中身の幅で折り返し、続きの行は欄の幅だけ空けて揃える。幅は unicode-width で数える
  - 追加と削除の行は、その行の最後の背景の色で行末まで埋める。delta は `ESC[0K` で行末まで塗るが、ansi-to-tui はこれを捨てるため
  - 幅が変わっても delta は呼び直さない。delta の出力を覚えておき、折り返しだけをやり直す
  - 確かめたこと (2026-11-23、delta 0.20.1、ansi-to-tui 7.0.0。実際の `structuredPatch` から組み立てた diff で):
    - 出力先がパイプでも色が付く
    - 上下に並べる表示では、`--width` も `--wrap-max-lines unlimited` も折り返しに効かない (折り返しは side-by-side だけ)。長い行は幅を超えたまま出る
    - ansi-to-tui は 24 ビット色と 256 色を読み、制御文字を文字として漏らさない。日本語の幅も正しく数える。`ESC[0K` は捨てる
    - 見出しが無い diff では色が付かない
    - `--file-style omit --hunk-header-style omit` で見出しが消え、区切りの間に空行が残る
    - 書式を `{nm:>6} ` と `{np:>6} │` にすると、6 桁の行番号でも欄の幅が揃う
    - 削除の行には構文の色が付かず、語の単位の差分 (変わった語の濃い背景) は付く

#### 作業履歴のキー

| キー | 一覧 | 展開 |
|---|---|---|
| `j` / `k` | 前後の行へ | 展開したまま前後の行へ |
| `l` / `→` | 展開する | なし |
| `h` / `←` | なし | 一覧に戻る |
| `{` / `}` | 前後の指示の行へ (要約の区切りの行では止まらない) | 同じ (展開したまま) |
| `ctrl-d` / `ctrl-u` | 半画面ずつ動く | 中身をスクロール |
| `gg` / `G` | 先頭 / 末尾 | 同じ |
| `o` / `Enter` | その行が触ったファイル (Read、Edit、Write のパス) を nvim で開く。ファイルの無い行では何もしない | 同じ |
| `O` | 同じファイルを専用のツールで開く。HTML のファイルと、URL を持つ行 (WebFetch、Artifact) はブラウザーに開かせる | 同じ |

### ファイルの表示のキー

| キー | 動き |
|---|---|
| `j` / `k`、`↓` / `↑` | 1 行ずつスクロール |
| `ctrl-d` / `ctrl-u` | 半画面ずつ |
| `gg` / `G` | 先頭 / 末尾 |
| `o` / `Enter` | nvim で開く |
| `O` | 専用のツールで開く。CSV は csvlens、TSV は `csvlens -t`、JSON は fx、HTML はブラウザー (`file://`)、それ以外は nvim |

共通: `Tab` でファイルの表示と作業履歴を切り替える (段階 2)。`q` は終了しない (常駐のため。誤って閉じないよう、終了は `ctrl-c` だけにする)。

### 隠れている間

- herdr では、preview の枠を git graph の mod と `stack` で切り替える。隠れている間も `wtr` のプロセスは動き続ける (2026-11-20 の試作で確かめた)。状態ファイルと jsonl の読み込みは続け、表示に戻ったときに最新になっているようにする
- herdr では、preview の枠ごと隠すこともある (`layout.toggle:preview-area`)。そのときも同じく読み込みを続け、表示に戻ったら描き直す。herdr のターミナルのプロセスは窓ではなく herdr-core (hc) が持ち、窓を閉じても動き続ける。どこにも表示していない hc のターミナルでもプロセスが止まらないことを、2026-11-20 に確かめた。そのため、枠ごと隠してもプロセスは動き続ける
- 画面の大きさが変わったとき (表示に戻ったときを含む) は、画像も含めて描き直す (設計者の判断)

### 外のツールを開く

- 開くときは TUI を一時停止し (画面を元に戻して)、同じターミナルでツールを動かす。ツールが終わったら TUI に戻り、画面を描き直す
- 作業ディレクトリは、ファイルのあるリポジトリの根 (分かれば)、無ければファイルのフォルダ

ブラウザーに開かせるとき (TUI は止めない):

- `TERM_PROGRAM` を外して `terminal-browser ls --all --json` で動いているブラウザーを探し、`wtr` 専用のタブ 1 つにページを開く。ローカルのファイルは `file:///abs/path` にする
  - 専用のタブを分けるのは、エージェントがテストに使うタブを上書きしないため (人とエージェントが同じブラウザーを使うので、アクティブなタブを開き直すと互いのページを消し合う。2026-11-23 の判断)
  - 初回は `terminal-browser action --browser <key> -- open <url>` で新しいタブを作り、返事の `openedTab` (tab id) をブラウザーの key と組で覚える
  - 2 回目からは `terminal-browser action --browser <key> --tab <id> --follow -- goto <url>` で、そのタブだけを開き直す。`O` を何度押してもタブは増えない。`--follow` が無いと、裏のタブを開き直すだけで、ブラウザーには前に出ているタブ (エージェントのタブなど) が出たままになる (コードで確かめた)。`--tab <id> --follow -- goto <url>` で裏のタブが前に出て開き直されることを、2026-11-23 に Herdr.app で確かめた
  - 呼んだあとに `terminal-browser action --browser <key> done` を呼び、ブラウザーに出る「エージェントが操作中」の印を消す (`action` は誰が呼んでもその印を出すため)
  - エージェントとの取り合い: agent-browser の操作の相手はブラウザーごとに 1 つで、`--tab` を付けない `action` は前に出ているタブに効く。`wtr` が `--follow` で自分のタブを前に出すと、そのあとエージェントが `--tab` なしで操作したとき、`wtr` のタブを触る。そのため、エージェントは自分のタブを `--tab` で指定する決まりにする (エージェントに教える skill に書く。dotfiles Issue #91 の受け持ち)
  - 覚えたタブが `ls` に無い (閉じられた)、またはブラウザーの key が変わったときは、初回と同じく作り直す
  - 覚えるのはプロセスの中だけ (`wtr` を起動し直したら、新しいタブを作る)
  - 専用のタブは状態ファイルの `tab` の値ごとに 1 つ持つ (タブごとの表示の位置と同じ考え方。2026-11-23 の判断)。`tab` の値が変わったら、その値のブラウザーのタブがあれば前に出す (`--follow` は `--` のあとに命令が要るので、ページを変えない命令 `get url` を添える) (ブラウザーを見ている最中にページが変わってよい)。無ければ何もしない (`O` を押したときに作る)
- ブラウザーが 1 つも無いときは `no browser` と出して何もしない。複数あるときは最初の 1 つにする (設計者の判断)
- 開いたあと、`--on-browse <cmd>` が渡されていればそれを実行する (herdr の中では、dotfiles が preview の枠をブラウザーに切り替えるコマンドを渡す)。無ければ何もしない
  - herdr の中での実現の見込み: `stack` の切り替えは Herdr.app の命令 `layout.select {stack, child}` で、`hc` には無い。外から直接は呼べないので、`--on-browse` のコマンドは mod が `watchFolder` で見張るフォルダにファイルを書き、mod が `layout.select` を呼ぶ形になる (2026-11-23 に Herdr.app の資料で確かめた。動かしてはいない。作るのは dotfiles)
- terminal-browser の呼び方 (`TERM_PROGRAM` の外し方、相手の選び方) は版で変わりうるので、1 か所にまとめる

### 見た目

- 色は catppuccin-macchiato。syntect のテーマ、状態のマーク、指示の行、差分の色をこれに揃える
- アイコンと固定の文言は上の決まりのとおり (Nerd Font のマークと英語)

## 作る人に任せること

- crate の構成、ファイルの分け方
- 状態ファイルの見張り方 (notify などで見張る、まとめて受ける間隔)
- 大きなファイルの色付けを後回しにする方法と、色なしに切り替える大きさ
- 折り返しと行番号の描き方
- 差分の色の具体的な値 (catppuccin-macchiato の中から選ぶ)。delta の出力を描く方法は「Edit の差分」で決めた
- 作業履歴の補足 (件数、時間、トークン数) を取る jsonl の場所。実際の jsonl を見て確かめる
- 試験。状態ファイルの読み取り、種類の分け方、CSV の読み取り、jsonl から行を作る処理、追記の読み取り、展開の位置の計算、`structuredPatch` から diff を組み立てる処理、delta の出力の折り返しと行末の塗りは、単体の試験で確かめる
