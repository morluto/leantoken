<!-- ルート README と同期して更新。技術仕様の正本は英語版です。 -->
<div align="center">

# LeanToken

**明示的なソーストークン予算を持つ、エージェント向けコードインテリジェンス。**

`mcp-name: io.github.morluto/leantoken`

**言語：** [English](../../../README.md) · [简体中文](../zh-CN/README.md) · 日本語 · [한국어](../ko-KR/README.md)

<img src="../../../assets/leantoken-hero-v3.jpg" alt="リポジトリをエージェントが必要とするコードへ絞り込む LeanToken" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#ライセンス)

</div>

LeanToken はローカルで不変のリポジトリ generation を構築し、同じ generation から
上限付きの検索、アウトライン、読み取りを提供します。CLI と MCP サーバーは同じ
アプリケーションサービスを呼び出し、ソースはローカルに残ります。

英語版 README とそこから参照される文書が、完全で正本の技術仕様です。このページは日常利用に
必要な現在の内容を日本語でまとめています。

## クイックスタート

対応するコーディングクライアントを設定します。

```bash
npx leantoken setup
```

クライアントを再起動し、リポジトリ内で接続を確認します。

```bash
npx leantoken doctor
```

CLI を直接使うこともできます。

```bash
npx leantoken refresh
npx leantoken search RepositoryGeneration
npx leantoken outline src/storage/snapshot.rs
npx leantoken read src/storage/snapshot.rs --lines 1:120
```

`--allow-broad-root` を明示しない限り、LeanToken はファイルシステムのルート、ホーム
ディレクトリ、およびその親を拒否します。セットアップは変更対象を表示して確認を求め、
自動実行ではクライアントを明示的に選ぶ必要があります。

## 取得モデル

`refresh` が公開境界です。

```text
リポジトリファイル -> 上限付き取得 -> 完全な派生 generation
                    -> 原子的な公開 -> search / outline / read
```

一回の取得は一つのコミット済み generation だけを観測し、二つの公開結果を混在させません。
ウォッチャーや互換性のための調停モードは更新を要求できますが、正しさはファイルシステム
イベントとの競争に依存しません。作業ツリーを変更したら `refresh` を実行してください。

公開 `read` はインデックス済み generation の内容を返します。未コミットの作業ツリーのバイトを
意図して読むライブラリ利用者は、より弱い保証を理解した上で明示的に
`Services::read_worktree` を使います。

MCP プロセス一つはリポジトリルート一つを担当します。複数のリポジトリには複数のプロセスを
起動し、リソースと障害を明確に分離します。

## 利用可能なツール

| ツール | 用途 |
| --- | --- |
| `leantoken.files` | ソースを読み込まず、インデックス済みパスを見つけます。 |
| `leantoken.search` | テキスト、正規表現、識別子、シンボル、参照を検索します。 |
| `leantoken.outline` | 定義、シグネチャ、インポート、範囲を確認します。 |
| `leantoken.read` | インデックス済みの正確な範囲、シンボル、見出しを読みます。 |
| `leantoken.history` | 上限付きの Git 履歴と差分を調べます。 |
| `leantoken.json` | 上限付きのライブ JSON 構造を照会します。 |
| `leantoken.context` | タスクに対する順位付き証拠を組み立てます。 |
| `leantoken.receipt_rebase` | 不変の証拠を新しい generation へリベースします。 |
| `leantoken.savings` | 取得とトークンの観測値を報告します。 |

`files`、`search`、`outline`、`read` が取得カーネルです。`context` はその原語を
オーケストレーションします。Git、JSON、セットアップ、更新、キャッシュ管理、オフライン分析は
それぞれ別の所有者を持ちます。

## トークン、カーソル、ライフサイクル

ソース予算とシリアライズ済み応答の予算は別です。`max_tokens` は返すソースを、
`max_response_tokens` は完全な JSON 応答を制限します。ページングカーソルはリポジトリ、
generation、正規化済み要求、位置に束縛されるため、別の内容や引数へ黙って継続できません。
クライアントは既に持つ内容のハッシュを渡して再送を避けられます。取得証拠と検索証明は、
可変セッションではなく不変で内容アドレス可能な成果物です。

セットアップ、キャッシュ、プライベートランタイムの削除は `--dry-run` で事前確認できます。
管理対象のクライアント設定は選択したバージョンに固定され、自動更新されません。

```bash
npx leantoken setup --claude --codex --yes
npx --yes leantoken@latest setup --refresh --yes
```

## ドキュメント

| 文書 | 内容 |
| --- | --- |
| [利用方法](../../usage.md) | 現在の CLI と MCP の動作 |
| [アーキテクチャ](../../architecture.md) | generation、ストレージ、上限の契約 |
| [開発](../../development.md) | テストの所有権と貢献手順 |
| [ベンチマーク](../../../benchmarks/README.md) | 任意の評価ツールと証拠ポリシー |
| [リリース](../../releases.md) | 公開と復旧の手順 |
| [変更履歴](../../../CHANGELOG.md) | リリース済みの変更 |

計画は GitHub issue と pull request で管理します。過去の設計や実験の説明は、現行文書として
維持するのではなく Git 履歴に残します。

## ライセンス

MIT License または Apache License 2.0 のいずれかを選択して利用できます。
