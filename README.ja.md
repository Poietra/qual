# Qual

**Manimを理解するlinter。壊れたレンダリングを、レンダリング前に。**

[![Release](https://img.shields.io/github/v/release/Poietra/qual)](https://github.com/Poietra/qual/releases/latest)
[![PyPI](https://img.shields.io/pypi/v/qual-manim)](https://pypi.org/project/qual-manim/)
[![crates.io](https://img.shields.io/crates/v/qual)](https://crates.io/crates/qual)
[![CI](https://github.com/Poietra/qual/actions/workflows/ci.yml/badge.svg)](https://github.com/Poietra/qual/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/Poietra/qual)](LICENSE)

[ドキュメント](https://poietra.github.io/qual/) ·
[全ルール](https://poietra.github.io/qual/rules/) ·
[English](README.md)

Manimコードに対するRuffのようなフィードバックに、`Scene.play`、Mobjectの
ライフサイクル、updater、Cairo/OpenGL、毎フレームの描画コストへの理解を
加えた静的解析ツールです。ManimもSceneも実行せず、実行時エラー・気づきにくい
誤描画・性能問題を検出します。

```bash
uv tool install qual-manim
qual check .
```

> RuffやPyrightはPythonを検査し、QualはManimがそのコードをどう扱うかを検査します。

## 一般的なPython linterには分からない問題

```python
from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        tracker = ValueTracker(0)
        label = always_redraw(
            lambda: MathTex(f"x={tracker.get_value():.2f}")
        )
        self.add(square, label)
        self.play(square.shift(RIGHT))
        self.play(tracker.animate.set_value(8), run_time=8)
```

```console
$ qual check . --format concise
scene.py:9:21: MLP226 warning Each invocation constructs a `MathTex` and performs a cache-key lookup, and this f-string key varies per frame: every rendered frame can mint a distinct Text/TeX cache key and disk asset (`K_resource ≈ F`). Across the 1 play(s) where this callback provably executes it may create at least ~480 distinct keys.
scene.py:12:19: MLC102 error `square.shift(...)` mutates the mobject immediately and returns the mobject itself, not an Animation; use `.animate` (e.g. `square.animate.shift(...)`) inside `Scene.play()`.
```

`MLC102`はレンダリングを中断させます。`MLP226`は毎フレーム異なるTeXアセットを
作る可能性があります。QualはManimのライフサイクルを追い、「一度だけ」と
「毎フレーム」を区別するため、両方を検出できます。フレーム数などの数値は、
ソースと選択したレンダープロファイルから証明できる場合だけ表示します。

## 検出するもの

Qual 0.3は、4ファミリーに分かれた**92個の実装済みManim固有ルール**を
提供します。

| ファミリー | 検出対象 |
| --- | --- |
| **MLC — ライフサイクルと正しさ** | 不正なAnimation、target/state不足、updater、同時書き込み、Scene所属の誤り |
| **MLR — レンダリング** | TeX、asset、geometry、描画順、camera、Cairo/OpenGL互換性による誤描画 |
| **MLP — パフォーマンス** | 毎フレームの構築、成長するScene graph、重いcallback、rasterやresource keyの乗数 |
| **MLD — 決定性と可搬性** | FPS依存の動き、未seed乱数、platform path、font、frame callback内の外部状態 |

各診断は**severity**（`error` / `warning` / `info`）と**confidence**
（`certain` / `high` / `medium` / `low`）を分けて持ちます。静的に解決できない
挙動は`Unknown`にし、不確実性から高確度の警告を作りません。

[全ルールと正確な検出範囲を見る →](https://poietra.github.io/qual/rules/)

## インストール

PyPIパッケージはRust製ネイティブ実行ファイルをインストールします。利用時に
Manim、LaTeX、Pythonランタイムは必要ありません。

```bash
# Pythonツールとして
uv tool install qual-manim
# または
pipx install qual-manim

# Rustツールとして（Rust 1.85以上）
cargo install qual --locked
```

Linux・macOS・Windows向けのstandalone installerとchecksum付きarchiveは、
各[GitHub Release](https://github.com/Poietra/qual/releases/latest)にあります。

```bash
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Poietra/qual/releases/latest/download/qual-installer.sh | sh
```

## ローカルとCIで使う

```bash
qual check .                         # richな端末表示
qual check . --format concise        # 1診断1行
qual check . --format github         # GitHub Actions annotation
qual check . --format sarif          # SARIF 2.1.0
qual check . --fix                    # safe fixのみ
qual cost scenes/demo.py             # Sceneごとの記号的コスト
qual coverage .                      # 解決できなかった解析範囲
qual explain MLC102                  # 1ルールの完全な説明
```

終了コードは、`0`が失敗閾値未満、`1`が閾値に達する診断あり、`2`が入力・設定・
内部エラーです。既存プロジェクトではbaselineを使って新規診断だけを検出できます。

```bash
qual check . --write-baseline .qual-baseline.json
qual check . --baseline .qual-baseline.json
```

設定は`pyproject.toml`の`[tool.qual]`に置きます。profileにrenderer、platform、
解像度、FPSを定義できます。詳細は
[設定ガイド](https://poietra.github.io/qual/guides/configuration/)を参照してください。

## Ruff・Pyrightとの役割分担

```bash
ruff check .
pyright
qual check .
```

- Ruff: style、import、Python一般のlint
- Pyright: Pythonの型
- Qual: Manimのライフサイクル、描画意味論、描画コスト

QualはRuff pluginではありません。Manim固有の問題は、複数文、helper、Scene所属、
Animationのsetup/cleanup、render profileをまたいで解析する必要があるため、独立した
解析器として動作します。

## ドキュメントと機械向けAPI

[ドキュメントサイト](https://poietra.github.io/qual/)では次を横断検索できます。

- インストール、設定、CI、baseline、suppression、fix
- 92ルールすべての根拠とnear-miss
- `qual cost`と`qual coverage`
- architecture、実測根拠、contributor向け資料
- versioned JSON contractとschema

公開される連携面はCLIとversioned JSONです。ネットワークAPIや公開Rust libraryでは
ありません。

```bash
qual check . --format json
qual static-facts .
qual change-impact --before old-tree --after new-tree
qual source-bridge . --request request.json
```

[機械向けAPI概要](https://poietra.github.io/qual/reference/machine-api/)から各RFCと
JSON Schemaへ移動できます。

## 安全性と現在の範囲

- Manim、plugin、解析対象コードをimport・実行しません。
- 現在のknowledge profileは**Manim Community 0.20**を対象にします。
- asset検査はManimの探索順をモデル化し、lint実行環境のfilesystemを参照します。
- 動的Pythonは保守的に扱い、`qual coverage`で未解決範囲を確認できます。
- Python一般のstyleや型エラーはRuff・Pyrightに任せます。

## 開発への参加

[`DESIGN.md`](DESIGN.md)が意味モデル・ルールカタログ・公開contractの正典です。
[`CONTRIBUTING.md`](CONTRIBUTING.md)にrepository構成、ルール追加、test gate、
knowledge profile更新手順があります。

```bash
cargo fmt --check
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

## ライセンス

Qualは[MIT License](LICENSE)で配布します。binary distributionに含まれる
LGPL対象依存関係については[`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md)と
[`RELINKING.md`](RELINKING.md)を参照してください。

Qualは独立プロジェクトです。Manim CommunityとRuffは本プロジェクトに関与せず、
責任を負いません。
