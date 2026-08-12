<!-- Translated from README.md at d1f1bdcc10e38894b0151c67f80990199305bf26. -->
<div align="center">

<h1>LeanToken</h1>

**AI コーディングのすべてのトークンを、もっと有効に**

コーディングエージェント向けのローカルファーストなコードインテリジェンス。
CLI と MCP サーバーを通じて、コード検索、構造の確認、正確な範囲の読み取り、
Git 履歴の調査を行えます。

**言語：** [English](../../../README.md) · [简体中文](../zh-CN/README.md) · 日本語 · [한국어](../ko-KR/README.md)

<img src="../../../assets/leantoken-hero-v3.jpg" alt="大規模なコードベースから AI エージェントに必要なファイルとコードを絞り込む LeanToken" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![npm downloads](https://img.shields.io/npm/dm/leantoken?logo=npm&label=downloads)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#ライセンス)

[クイックスタート](#クイックスタート) · [LeanToken を選ぶ理由](#leantoken-を選ぶ理由) · [ツール](#利用可能なツール) · [CLI](#cli-の使い方) · [仕組み](#仕組み) · [ドキュメント](#ドキュメント)

</div>

---

> **翻訳について：** 最新かつ完全な仕様は[英語版 README](../../../README.md)です。
> このページでは、導入と日常利用に必要な主要事項を説明します。バージョン、安全性、
> 高度な取得動作については英語版ドキュメントを参照してください。

> **計測済みのトークン削減：** 60 回の制御された実験では、エージェントの組み込み
> ツールと比べ、限定的なリポジトリ探索でモデル入力トークンを 20.1%、広範な探索で
> 37.6% 削減しました。詳細は[計測方法](../../measurement.md)を参照してください。

## クイックスタート

Claude Code、Cursor、OpenCode、Codex、Gemini CLI、または Antigravity に
LeanToken を追加します。

```bash
npx leantoken setup
```

セットアップウィザードは検出したクライアントを表示しますが、初期状態では何も選択
しません。LeanToken を追加するエージェントを自分で選び、書き込み前に設定先と MCP
ランチャーを確認できます。`npx` でセットアップすると、実行時の正確なバージョンが
固定されるため、クライアントの再起動だけで暗黙に更新されることはありません。

設定したクライアントを再起動または再読み込みし、リポジトリ内で接続と最初の取得を
確認します。

```bash
npx leantoken doctor
```

たとえば、*編集する前にリクエストのキャンセルに関係するコードを探して* と依頼して
みてください。LeanToken はエージェントが `leantoken.context` から始められるように
し、編集、ビルド、テストには通常のツールをそのまま使えます。

観測されたリポジトリ単位のトークン使用量を確認します。

```bash
npx leantoken savings
```

| 特長 | 内容 |
| --- | --- |
| **ローカルが既定** | ソースはローカルデータベースに索引化されます。LeanToken は読み取り専用の探索・取得レイヤーです。 |
| **明示的なトークン予算** | すべての応答に上限があり、大きなファイルがリクエスト全体を占有しません。 |
| **エージェント向けワークフロー** | ファイル探索、コード検索、構造確認、範囲読み取り、履歴追跡、JSON 問い合わせ、使用量計測を専用ツールで行えます。 |

### 自動セットアップと削除

ウィザードを省略してクライアントを明示するか、対応する全クライアントを設定します。

```bash
npx leantoken setup --claude --codex --yes
npx leantoken setup --all --yes
```

ファイルを変更せずに同じ設定プランを確認できます。

```bash
npx leantoken setup --codex --cursor --dry-run
```

LeanToken が管理する統合を削除します。

```bash
npx leantoken remove
```

## よく使うエージェントワークフロー

LeanToken は、リポジトリ全体を一度に渡すのではなく、小さな証拠のループとして使うと
効果的です。

1. **自律的な方向付けを 1 回で行う。** 不確実で広範なタスクでは `context` と
   `plan_only: false` から始め、返されたソースをそのまま使います。実装または
   回帰テストの担当が欠けていることをカバレッジが明示した場合に限り、焦点を
   絞った追加呼び出しを最大 1 回行います。高コストまたは高リスクな取得を人が
   確認する場合は、引き続き `plan_only: true` でプレビューできます。
2. **ソースを再送せず続ける。** 次の context 呼び出しに前回の `receipt_id` を渡すか、
   返されたフラグメントハッシュを `known_hashes` として渡します。
3. **観測済みの失敗を調査する。** `investigation` ワークフローを使い、
   `workflow_evidence` には直接観測した失敗、パス、シンボル、テスト意図だけを渡します。
4. **変更をレビューする。** `review` ワークフローで `base_revision` を
   `BASE..HEAD`、`strict_changed_paths` を `true` に設定します。

## LeanToken を選ぶ理由

多くのエージェントは広く検索してからファイル全体を読みます。LeanToken はその作業を
段階的に絞り込みます。

| 一般的なリポジトリ探索 | LeanToken を使用 |
| --- | --- |
| 大量のディレクトリ一覧を走査 | コンパクトなツリーから関連パスを発見 |
| 構造を知るためファイル全体を読む | ファイル全体を読み込まず定義と import を確認 |
| ターンごとに同じコードを再送 | 未変更の証拠の重複を回避 |
| 大きなファイルが応答を占有 | ソースを正確な予算内に収め、応答オーバーヘッドを別途報告 |
| 重要なファイルを推測 | タスクとの関連度でコードを順位付け |

編集、コマンド、テスト、会話は引き続きコーディングエージェントが担当します。
LeanToken は、それらに必要なコードを見つけて返します。

## 利用可能なツール

| ツール | 用途 |
| --- | --- |
| `leantoken.context` | 広いタスクの既定の入口。トークン予算内で順位付き証拠をプレビューまたは取得します。 |
| `leantoken.search` | テキスト、正規表現、識別子、シンボル、参照を順位付きで検索します。 |
| `leantoken.files` | ignore 設定を尊重したコンパクトなパス探索です。 |
| `leantoken.outline` | ファイル全体を読まず、定義、シグネチャ、import、範囲を確認します。 |
| `leantoken.read` | 1 つの正確なシンボルまたは包含行範囲を読みます。 |
| `leantoken.history` | 不変な Git リビジョン間で解析済みシンボルを読み、比較し、追跡します。 |
| `leantoken.json` | 有界な現在の JSON を問い合わせ、要約、比較します。 |
| `leantoken.savings` | 応答統計、ハッシュ抑制、失敗、観測上限を報告します。 |

## CLI の使い方

`npx` から直接実行します。

```bash
npx leantoken status
npx leantoken savings
npx leantoken doctor
npx leantoken --root /path/to/repo search handle_request
```

またはグローバルバイナリをインストールします。

```bash
npm install --global leantoken@latest

leantoken --root /path/to/repo index
leantoken --root /path/to/repo search handle_request --mode identifier --max-tokens 800
leantoken --root /path/to/repo context \
  --task "fix request cancellation during shutdown" \
  --budget 2000
```

## インストールと更新

npm パッケージには次のネイティブバイナリが含まれます。

- macOS（ARM64、x64）
- glibc Linux（ARM64、x64）
- Windows（x64）

musl Linux を含むその他のターゲットではソースからのビルドが必要です。Rust 1.95
以降とネイティブ C/C++ ツールチェーンをインストールして、次を実行します。

```bash
cargo install --git https://github.com/morluto/leantoken --package leantoken leantoken
```

既存のクライアント統合を明示的に更新します。

```bash
npx --yes leantoken@latest setup --refresh --yes
```

固定された MCP 設定が暗黙に `@latest` へ移動することはありません。ロールバック、
キャッシュ、バージョンの詳細は[利用ガイド](../../usage.md)を参照してください。

## 仕組み

```text
リポジトリ
    │
    ▼
ファイル探索 ──► コード構造の抽出 ──► ローカル検索インデックス
                                          │
                                          ▼
エージェント要求 ──► 順位付き / 正確な取得 ──► 予算内の対象コード
```

LeanToken はソースを一度索引化し、コンパクトなパス、順位付き一致、構造アウトライン、
正確なソース範囲、タスク固有のコンテキストを提供します。ターン間で未変更の証拠を
再送することも避けます。

## ドキュメント

| ガイド | 内容 |
| --- | --- |
| [利用方法とツールリファレンス](../../usage.md) | コマンド、MCP ツール、要求オプション、例 |
| [アーキテクチャと信頼性](../../architecture.md) | コンポーネント、データフロー、ストレージ、障害時の動作 |
| [ロードマップ](../../roadmap.md) | 現在の方向性と計画 |
| [開発とテスト](../../development.md) | ローカル設定、検証、リリースフロー |
| [ベンチマーク手法](../../../benchmarks/README.md) | トークン効率の計測と解釈 |
| [計測ハーネス](../../measurement.md) | 実験、通信コスト、プロファイリングツール |

## ライセンス

次のいずれかを選択できます。

- [Apache License, Version 2.0](../../../LICENSE-APACHE)
- [MIT License](../../../LICENSE-MIT)
