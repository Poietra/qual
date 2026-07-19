# manim-lint

**[Manim Community](https://www.manim.community/) のシーンを静的解析 — レンダリング前に、確実な実行時エラー・意図と異なる描画・性能の乗数・非決定性を検出する。**

[English](README.md) | 日本語

`manim-lint` は Manim Community **0.20** 系プロジェクト向けの独立した静的解析器で、Rust で実装されています。Python ソースをパースし、検証済み・バージョン管理された Manim 意味モデルと照合します。Manim も解析対象コードも **決して import・実行しません**。API 名のパターンマッチではなく、`Scene.play` の実際の挙動(引数のコンパイル、auto-add、introducer / remover、updater)を再現するライフサイクル抽象解釈器と、「どのコードが 1 回だけ実行され、どのコードが毎フレーム実行されるか」を把握する記号的コストモデルの上で診断します。

## 例

`scenes/demo.py`:

```python
from manim import *


class TrackerDemo(Scene):
    def construct(self):
        title = Text("Tracking x", font_size=0)
        square = Square()
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f"x={tracker.get_value():.2f}"))
        self.add(title, square, label)
        self.play(square.shift(RIGHT))
        square.add_updater(lambda m: m.rotate(0.05))
        self.play(tracker.animate.set_value(8), run_time=8)
        self.wait(0)
```

```console
$ manim-lint check .
scenes/demo.py:6:46: MLR115 error `Text(font_size=0)` is not positive; text sizing requires font_size > 0
scenes/demo.py:9:39: MLP226 warning Each invocation constructs a `MathTex` and performs a cache-key lookup, and this f-string key varies per frame: every rendered frame can mint a distinct Text/TeX cache key and disk asset (`K_resource ≈ F`). Across the 1 play(s) where this callback provably executes it may create at least ~480 distinct keys.
scenes/demo.py:11:19: MLC102 error `square.shift(...)` mutates the mobject immediately and returns the mobject itself, not an Animation; use `.animate` (e.g. `square.animate.shift(...)`) inside `Scene.play()`.
scenes/demo.py:12:38: MLD301 warning Updater lambda applies `rotate` with a fixed step every frame but declares no `dt` parameter; the motion speed depends on the profile frame rate
scenes/demo.py:14:9: MLC112 warning This `wait()` renders a single frozen frame: nothing makes it dynamic, and the updater registered at line 9 reads frame-varying state without a `dt` parameter, so its visual change never renders during the wait. Pass `frozen_frame=False`, or declare a `dt` parameter on the updater.
scenes/demo.py:14:19: MLC104 error Use a positive `duration`: the literal `0` is non-positive and playing it aborts the render.
```

このうち 2 件はレンダリングをクラッシュさせ(`MLC102`、`MLC104`)、1 件はタイトルを不可視のまま描画し(`MLR115`)、1 件はフレームレートによって動きの速さが変わり(`MLD301`)、1 件は動くはずの wait を凍結し(`MLC112`)、1 件は毎フレーム新しいキャッシュキーで外部 TeX コンパイラを起動します(`MLP226`)— しかもそのコストは定量化されます: 60 FPS で `run_time=8` なら少なくとも約 480 個の相異なるキーであることが証明できます。

## 検出できるもの

ルールは 4 つのファミリーに分かれます。

- **MLC — ライフサイクル / 正しさ。** 確実な実行時エラーと、誤った絵を描くライフサイクル上の誤り。`Scene.play` への非 Animation 引数(`MLC102`)、全パスで `generate_target()` を欠く `MoveToTarget`(`MLC107`)、`save_state()` なしの `Restore`(`MLC120`)、一つの play 内で同じ mobject の同じチャンネルへ書き込む 2 つのアニメーション(`MLC108`)、生き残った親の再追加で無効化される `Scene.remove(child)`(`MLC115`)。
- **MLR — レンダリング。** 描画はされるが意図した絵にならないコード。非 raw の `MathTex` リテラル内で TeX コマンドを壊す Python エスケープ(`MLR103`)、Manim の実際のランタイム探索で解決できないアセットパス(`MLR104`)、プレーンな `Text` に渡された Pango マークアップ(`MLR124`)、`Transform(mob, mob)`(`MLR113`)。
- **MLP — パフォーマンス。** 機械可読な根拠付きのコスト乗数。updater や `always_redraw` 内での `Text`/`MathTex`/`SVGMobject` 構築(`MLP201`)、毎フレーム 1 つのディスクアセットを生む frame 依存 TeX キャッシュキー(`MLP226`)、毎フレーム成長するシーングラフ(`MLP204`)、`dissipating_time` なしの `TracedPath`(`MLP220`)。
- **MLD — 決定性 / 可搬性。** マシン・フレームレート・レンダラーによって結果が変わるコード。`dt` でスケールされない固定の毎フレームステップ(`MLD301`)、フレームコールバック内の未シード global random(`MLD302`)、大文字小文字を区別するプラットフォームでの case-only アセットパス不一致(`MLD305`)。

### 名前照合ではなく意味の深さ

解析器の中心原則(DESIGN §1)は「API 名だけで警告しない」ことです。未追加の mobject への `FadeOut(mob)` は正しいコードです — play の準備段階で自動追加され、remover として終了後に削除されます。そのためパイプラインはまず実際の事実を構築します。

- **ライフサイクル抽象解釈器**: 関数内 CFG、手続き間ヘルパー要約、`super()` ディスパッチを含む Scene ごとの MRO 合成、割り当てサイト同一性、Scene 所属 / 順序 / updater の追跡、play グループの意味論。
- **記号的コストモデル**: hot context の伝播(updater、`always_redraw`、stop condition、interpolate オーバーライド)と、リテラルな duration からのみ導出されるフレーム数区間。上のコストレポートが `duration 8 s -> frames ~480` と言えるのは 60 FPS で `run_time=8` が証明できるからで、証明できない値は `unknown` と表示し、数値を捏造しません。

すべての診断は **severity**(`error`/`warning`/`info`)と **confidence**(`certain`/`high`/`medium`/`low`)を分離して持ち、状態依存ルールは全パスで確定した根拠でのみ発火します。静的に解決できない値は `Unknown` に落とし、**推測するより沈黙する** — この設計姿勢を全ルールで貫いています。

## インストール

Rust ツールチェーン(1.85+)が必要です。crates.io へのリリースはまだありません。ソースからインストールしてください。

```bash
git clone <this repository>
cd manim-lint
cargo install --path .
```

## クイックスタート

```bash
manim-lint check .                      # 解析、concise 出力
manim-lint check scenes --format full   # 説明と根拠つき
manim-lint check . --format json       # schemas/diagnostics-v1.json 準拠
manim-lint check . --format sarif      # SARIF 2.1.0
manim-lint check . --format github     # GitHub Actions アノテーション
manim-lint explain MLC102               # ルールの完全なドキュメント
manim-lint rules                        # 全ルール ID・フェーズ・実装状態
manim-lint config                       # 解決済みの有効な設定
manim-lint cost scenes/demo.py          # シーンごとのコスト内訳
manim-lint coverage .                   # 解析が解決できなかったものの一覧
```

終了コード: `0` — `fail-level` に達する報告済み診断なし。`1` — 1 件以上あり。`2` — コマンドライン / 設定 / 内部エラー。

主な `check` オプション: `--select` / `--ignore`、`--min-confidence`、`--fail-level`、`--profile`、`--renderer`、`--fps`、`--resolution WIDTHxHEIGHT`、`--statistics`、`--analysis-summary`(後述の解析カバレッジレポートを診断の後に stderr へ出力。stdout と終了コードは変化しない)、および後述の baseline / fix オプション。`--no-cache` はキャッシュを読み書きせず、cache directory も作らずに完全解析を強制します。`--select` は解析そのものも絞り込みます: 選択したルールが必要としない事実レイヤー(ライフサイクル解釈器、記号的コストモデル)の計算をスキップするため、狭い select はフル実行より高速です。報告される診断はどちらでも同一です — 選択したルールを supersede するルールは常に実行されるので、狭い select が supersede 済みの診断を復活させることはありません。

`--format full` は各診断の下に説明と機械可読な根拠を表示します。

```text
scenes/demo.py:9:39: MLP226 warning Each invocation constructs a `MathTex` and performs a cache-key lookup, ...
    A frame-varying key defeats the `MathTex` cache: instead of one shaping/compile job reused every frame,
    each frame pays construction plus a cache miss, and for TeX classes each distinct key also launches the
    external TeX compiler and `dvisvgm`, leaving one disk asset per key. ...
    evidence.distinct_resource_keys: {"lower":480,"upper":null}
    evidence.execution: {"plays":[{"certainty":"proven","kind":"play","location":"scenes/demo.py:13:9"},{"certainty":"maybe","kind":"play","location":"scenes/demo.py:11:9"}],"unresolved_entries":false}
    evidence.frames: {"lower":480,"upper":null}
    evidence.invocation_context: "frame-callback"
    evidence.multiplicity: ["frames"]
    evidence.state_path: ["construct","always_redraw:9"]
    applies to profiles: production
```

## 解析キャッシュ

通常の `check` は破棄可能な SQLite cache を
`.manim-lint-cache/cache-v1.sqlite3` に保持します。初回はproject全体を
解析してfilter後のdiagnosticsをJSONで保存し、同一入力の二回目はfilesystem
dependencyを検証して結果を再利用します。keyにはanalyzer build、解決済みの
semantic config、Manim knowledge profile、対象source pathとsource bytesが
含まれます。literal asset候補とcase-sensitiveなdirectory walkは別にstamp
されるため、Python sourceが不変でもassetの作成・削除・rename・内容変更で
entryは無効になります。

DBはSQLite WALを使って並行cold writerを許容し、古い設定で際限なく増えない
よう直近に使った16個のproject snapshotだけを保持します。正しさに必要な状態
ではありません。破損はwarningを出してDBを削除・再構築し、完全解析を継続
します。`--no-cache` はcacheに関するfilesystem accessをすべて無効にします。
cache-v1ではlive source/index stateを後段でも使う`--fix`、baseline、
`--analysis-summary`も意図的に完全解析します。projectのignore fileには
`.manim-lint-cache/`を追加してください。

cold runは依存しないsummary component、Scene lifecycle、ruleをbounded worker
poolで並列化します。再帰summaryのfixpointとfrontend/project indexは順序を
保った逐次処理です。結果は安定sortされ、worker 1件と4件でJSONがbyte単位に
同一であることをtestしています。

## 設定

設定は `pyproject.toml` の `[tool.manim-lint]` から読み込みます(検査対象パスから上方向へ探索)。レンダープロファイルは `[[tool.manim-lint.profile]]` エントリです。

```toml
[tool.manim-lint]
manim-version = "0.20"
target-python = "3.11"
select = ["MLC", "MLR", "MLP", "MLD"]
ignore = []
min-confidence = "high"
fail-level = "warning"
default-profile = "production"
knowledge-profile = "upstream_0_20"
respect-manim-cfg = true
exclude = [".venv/**", "media/**"]
per-file-ignores = { "tests/fixtures/**" = ["MLP", "MLD"] }

[[tool.manim-lint.profile]]
name = "production"
renderer = "cairo"
platform = "linux"
pixel-width = 1920
pixel-height = 1080
frame-rate = 60
assets-dir = "."
allowed-fonts = ["Noto Sans", "Noto Sans CJK JP"]
```

優先順位(高い順):

```text
CLI > selected profile > pyproject base > manim.cfg > builtin defaults
```

`respect-manim-cfg` が有効(既定)なら、`manim.cfg` が解像度 / fps / レンダラーの既定値を pyproject 設定の下位として補います。未知のキー、未知のルールセレクター、重複するプロファイル名、未定義プロファイルへの参照は設定エラー(exit 2)です。`--profile all` は定義済みの全プロファイルを解析し、同じ根拠の診断を 1 件へ統合して、影響するプロファイルを診断ごとに列挙します。

設定は正直に検証されます(違反は exit 2):

- 宣言した `manim-version` は、設定した knowledge profile が対応する Manim 範囲内でなければなりません(例: `upstream_0_20` は `>=0.20,<0.21` に対応)。未宣言なら検証しません。
- `target-python` は `MAJOR.MINOR` 形式で 3.6〜3.12 の範囲に収まる必要があります。上限は同梱パーサー(rustpython-parser 0.4)が実装する Python 文法、下限は構文ゲートの完全性を保証できるフロアです(それより古い target は黙って放置されず exit 2 で拒否されます)。文法は固定で(`feature_version` の指定はなし)パース結果は変わりませんが、パース後のゲートが AST・トークン列・f-string テキストを走査し、target より新しい構文をすべて `MLC000` として報告します(`async def` 外の `async`/`await` 構文 3.7、`:=`・位置専用引数 `/`・f-string の自己文書化 `=` 3.8、拡張デコレーターと `as` 付き括弧付きコンテキストマネージャー 3.9、`match` 3.10、`except*` と PEP 646 の添字 `*` アンパック 3.11、`type` エイリアス・PEP 695 型パラメーター・PEP 701 f-string 式 3.12)。ゲートを無警告で通過したファイルは target 自身のパーサーで必ずパース可能です。ゲートされたファイルも解析は継続し、対象構文を持ち込む `--fix` はロールバックされます。カバレッジ表は `manim-lint explain MLC000` を参照してください。
- ゼロ・負・非有限のフレームレートと、寸法が 0 の解像度は、どの経路(`--fps` / `--resolution`、プロファイル、`manim.cfg`)から来ても拒否されます。
- `stub-paths` は未実装です。空でないリストは黙って無視されず、設定エラーになります。

`manim-lint config` は解決済み設定に加えて、どの設定が強制され、どれが情報提供のみかを示す `enforcement` セクションを出力します。

## 最適化フォークプロファイルの利用

ローカルにパッチを当てた Manim フォーク(プロファイル `local_0_20_1_4d25c031`)でレンダリングするプロジェクトは、それを manim-lint に伝えることでフォーク固有の解析レイヤーを有効化できます:

```toml
[tool.manim-lint]
knowledge-profile = "local_0_20_1_4d25c031"
default-profile = "production"

[[tool.manim-lint.profile]]
name = "production"
renderer = "cairo"
platform = "linux"
cairo-fork-workers = 4
cairo-static-layers = true
```

upstream プロファイルが提供するすべてに加えて、次が有効になります:

- **`manim-lint cost` の「fork fast paths」セクション**: play ごとに、fork-per-play Cairo パイプライン(`cairo-fork-workers`)、静的レイヤー保持パス(`cairo-static-layers`)、packed interpolation が適用されるかどうかを表示します。適用されない場合は正確なブロッカーとそのソース位置(例: Scene updater)を示し、最初の serial play 以降のレンダラー全体に及ぶ単調な無効化の因果連鎖も説明します。このセクションは機能の削除を助言することは決してなく、レンダーパス上の帰結を説明するだけです。
- **`MLP214`**: シーン最初の play より前に 4 個以上の相異なる TeX コンパイルキーが直列に構築される箇所を指摘し、フォークの事前コンパイル API(`MathTex.precompile`、`tex_to_svg_file_async`)を提示します。
- **`MLP217`**: hot なコールバック内でフレームごとに変わる `use_svg_cache=True` キーが、フォークが宣言するプロセスグローバル SVG キャッシュを毎フレーム成長させる箇所を指摘します。
- **`MLP225`**(`--select MLP225` によるオプトイン): cost レポートの fast-path ブロッカー説明を play ごとの診断として出力します。

`upstream_0_20` の下では上記はすべて不活性です: cost レポートに fork セクションは現れず、3 つのルールは選択されても決して発火しません。

## 抑制(suppression)

```python
self.play(square.shift(RIGHT))  # manim-lint: ignore[MLC102]   # 同じ文

# manim-lint: ignore[MLP201]                                   # 次の文
label = always_redraw(...)

# manim-lint: file-ignore[MLP]   # ファイル全体。ファイルヘッダー領域に置く
```

抑制の対象は行単位ではなく**文単位**です。行末コメント(または直上の独立コメント)は、複数行にまたがる呼び出しの継続行も含めた文全体をカバーし、その文の内部のどこに位置する診断でも抑制されます。複合文(`def`、`for`、`if`、`with` など)ではヘッダー(コロンまで)だけをカバーし、1 つのコメントがスイート全体を沈黙させることはありません。

インライン抑制内の未知のルール ID は何も抑制せず、専用の警告として報告されます。

```text
scene.py:8:41: MLC001 warning unknown rule ID in suppression: MLC999
```

ディレクトリ単位には `pyproject.toml` の `per-file-ignores` を使ってください(上の例を参照)。

## 段階導入: baseline

既存プロジェクトに、全部を直す前から導入できます。

```bash
manim-lint check . --write-baseline .manim-lint-baseline.json  # 今日の検出結果を記録
manim-lint check . --baseline .manim-lint-baseline.json        # 新規の検出だけを報告
```

baseline の指紋(`schemas/baseline-v1.json`)は **行番号を含みません** — ルール ID、相対パス、修飾された Scene 名、周辺トークンのハッシュから作られるため、ファイル内の無関係な行の追加でエントリが失効しません。`scene` フィールドには修飾された囲み Scene クラス名が記録され(Scene の外では空)、別々の Scene にある同一の検出は異なる指紋になります。書き出されるファイルには来歴マーカー `scene_attribution: "attributed"` が付き、そのファイルでは空の `scene` は文字どおり「どの Scene の外」を意味して厳密に一致します。Scene 帰属導入前に書かれた baseline(マーカーなし)も引き続き読み込め、その場合に限り空の `scene` がワイルドカードとしてマッチします。破損した、あるいはスキーマの合わない baseline ファイルは明確なメッセージとともに exit 2 になります。

## 自動修正(autofix)

```bash
manim-lint check . --fix            # SAFE な修正だけを適用
manim-lint check . --fix --unsafe-fixes  # UNSAFE な修正も適用
```

safe と unsafe は厳密に分離されています。`--fix` 単体では挙動を変えない編集だけを適用します(例: `MLC127` は 1 回の `add()`/`VGroup()` 呼び出しから重複した子を除去、`MLR104` は case-only のアセットパスを修正)。unsafe な修正は実行時の意味を変え得るため(例: `MLC102` の `play(mob.shift(...))` → `play(mob.animate.shift(...))` への書き換え)、明示的な追加フラグが必要です。修正されたファイルはすべて再パースで検証され、検証に失敗したファイルはロールバックされます。

```console
$ manim-lint check . --fix
scene.py:8:40: MLC127 info Remove the duplicate `square` from this `VGroup(...)` call: Manim warns and ignores repeated children of a single add.
fixed 1 issue(s) in 1 file(s)
```

## cost コマンド

`manim-lint cost` はシーンごとの記号的コスト内訳を表示します — フレーム数区間つきの play リスト、由来とコールバックの実行が証明された play つきの hot context、毎フレーム構築、リソースキーの成長。未知の duration は unknown と表示し、数値を捏造しません。

```console
$ manim-lint cost scenes/demo.py
profiles: production (cairo, 1920x1080, 60 fps)

scene scenes.demo.TrackerDemo (scenes/demo.py)
  plays:
    scenes/demo.py:11:9 play duration unknown -> frames per-frame
    scenes/demo.py:13:9 play duration 8 s -> frames ~480
    scenes/demo.py:14:9 wait duration 0 s -> frames ~0
  hot contexts:
    scenes/demo.py:9:31 entry always_redraw; path construct -> always_redraw:9; factors frames; proven execution plays: scenes/demo.py:13:9
    scenes/demo.py:12:28 entry updater; path construct -> updater:12; factors frames; proven execution plays: scenes/demo.py:13:9
  per-frame constructions:
    scenes/demo.py:9:39 MathTex construction x at least ~480 invocations across 1 proven play(s)
  resource-key growth:
    scenes/demo.py:9:39 MathTex distinct cache keys: at least ~480 across 1 proven play(s) (f-string key varies per frame)
```

ローカルフォークの knowledge profile の下では、レポートにシーンごとの「fork fast paths」セクションが加わります(上の「最適化フォークプロファイルの利用」を参照)。例えばプロファイルに `cairo-fork-workers = 4` と `cairo-static-layers = true` を設定した場合:

```console
$ manim-lint cost scene.py
...
  fork fast paths (profile production, knowledge local_0_20_1_4d25c031):
    fork-per-play (cairo_fork_workers 4):
      scene.py:9:9 play #1: no static blocker found (fork-eligible pending the runtime audit)
      scene.py:10:9 play #2: no static blocker found (fork-eligible pending the runtime audit)
    static layers (cairo_static_layers on):
      scene.py:9:9 play #1: no static blocker found
      scene.py:10:9 play #2: no static blocker found
    packed interpolation:
      scene.py:9:9 play #1: canonical per-member interpolation because the animation type FadeIn is outside the audited allowlist at scene.py:9:19 (blocker unsupported_animation_type); an updater-bearing mobject is in the scene family (updater registered here) at scene.py:8:9 (blocker updater_bearing_family)
      scene.py:10:9 play #2: canonical per-member interpolation because an updater-bearing mobject is in the scene family (updater registered here) at scene.py:8:9 (blocker updater_bearing_family)
      evidence: measured packed interpolation on the calibration machine, 300 members / 60 frames: 130.658 -> 33.004 ms/play, steady state 2.0761 -> 0.1890 ms/frame (docs/research/perf-evidence.md)
    note: the features named above can be correct expression; this section explains the render-path consequence and never advises removing them
```

## 解析カバレッジ

保守的な沈黙は正しくても見えません。クリーンな実行が「問題なし」なのか「半分しか解析できなかった」のか — その沈黙は安全なのか、それとも盲目なのか — を区別できるように、`manim-lint coverage`(および同じレポートを stderr に出す `manim-lint check --analysis-summary`)は解析が解決**できなかった**ものを列挙します: 未解決の import(不明モジュールからの star import、プロジェクト木を出る相対 import)、候補が空の呼び出し、duration 不明の play、対象不明の `.animate` ビルダー、`target-python` を超える構文(MLC000)、knowledge profile に無い manim API、コンストラクタ状態が不明なシーン、インライン化されずサマリーへフォールバックしたヘルパー呼び出し(再帰・解決不能。プロジェクト全体で重複排除して集計)。

解決できないモジュールからの star import、プロジェクト木を出る相対 import、`target-python = "3.9"` を超える `match` 文、未解決ヘルパー呼び出しに包まれた play を含むファイルの例:

```console
$ manim-lint coverage .
analysis coverage (knowledge profile upstream_0_20, target-python 3.9)

scene.py
  constructs above target-python (MLC000): 1
  star imports from unresolved modules: 1
  unresolved relative imports: 1
  calls with no resolved target: 1 of 6 (mystery x1)

scene scene.Demo (scene.py)
  plays with unknown duration: 1 of 2
  .animate builders with unknown target: 0 of 0

project
  files parsed: 1 of 1
  calls resolved: 5 of 6
  play durations known: 1 of 2
  scene constructors resolved: 1 of 1
  constructs above target-python (MLC000): 1
  unresolved imports: 2 (1 star, 1 relative)
  manim APIs not in the knowledge profile: 0
  helper calls summarized, not inlined: 0
  top unresolved calls: mystery x1

analysis confidence: 1/1 files parsed, 5/6 calls resolved, 1/2 play durations known, 1/1 scene constructors resolved (counts of analyzed facts, not estimates)
```

すべての数値は計算済みファクトの個数であり、比率は「解決済み / 総数」の単純なカウント対のみです。`--format json` は安定したトップレベルキー `knowledge_profile`、`target_python`、`files[]`(`path`、`parsed`、`gated_constructs`、`unresolved_star_imports`、`unresolved_relative_imports`、`calls`、`unresolved_calls`、`unresolved_call_names`、`apis_not_in_profile`)、`scenes[]`(`name`、`path`、`constructor_state_unknown`、`plays`、`plays_with_unknown_duration`、`builders`、`builders_with_unknown_target`)、`project`(総計に加えて `top_unresolved_call_names` と `helper_inline_fallbacks` — サマリーへフォールバックしたヘルパー呼び出しサイト数。ヘルパー連鎖を共有するシーン間で重複排除されるため `project` にのみ現れます)を持つ機械可読ドキュメントを出力します。出力は決定的で、同一入力に対してバイト単位で安定です。

## CI 連携

GitHub Actions アノテーションを PR の差分に直接表示する場合:

```yaml
name: manim-lint
on: [push, pull_request]
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install --path . --locked
        working-directory: manim-lint   # manim-lint checkout へのパス
      - run: manim-lint check . --format github
```

SARIF をアップロードして GitHub の code scanning UI に表示する場合:

```yaml
      - run: manim-lint check . --format sarif > manim-lint.sarif
        continue-on-error: true
      - uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: manim-lint.sarif
```

## ルールカタログ

4 ファミリーに 92 個のルール ID があり、**92 個すべて実装済み**です。

| ファミリー | 実装済み | reserved |
| --- | --- | --- |
| MLC ライフサイクル / 正しさ | 31 | 0 |
| MLR レンダリング | 27 | 0 |
| MLP パフォーマンス | 27 | 0 |
| MLD 決定性 / 可搬性 | 7 | 0 |

実装済みルールのうち 1 つはオプトインです: `MLP225` は `default_enabled: false` で、通常の `check` 実行には決して参加しません。ローカルフォークプロファイルの下で正確な `--select MLP225` を指定したときだけ評価されます。

ルールごとの状態・severity・confidence を含む完全な索引は [docs/rules/README.md](docs/rules/README.md) にあります。実装済みルールにはそれぞれドキュメントページがあり、`manim-lint explain <ID>` でも読めます。

## アーキテクチャ

```text
Python sources
   |
SourceManager ............ encoding (PEP 263), newlines, Unicode columns
   |
knowledge profile ........ versioned Manim 0.20 semantics (no import, ever)
   |
frontend ................. imports/aliases, project index, qualified call facts
   |
semantic ................. lifecycle abstract interpreter -> LifecycleFacts
   |
cost ..................... hot contexts, frame intervals -> CostFacts
   |
rules .................... MLC / MLR / MLP / MLD over the fact layers
   |
suppressions, supersedes, baseline
   |
output ................... concise | full | json | sarif | github, fixes, cost report
```

[docs/architecture.md](docs/architecture.md)(英語)は新しいコントリビューター向けにこのパイプラインを解説します: 各事実レイヤーが何を提供しどこにあるか、knowledge profile システム、そして 1 つの診断がエンドツーエンドでどう流れるか。意味モデル・ルールカタログ・公開契約の正典仕様は [`DESIGN.md`](DESIGN.md) です。JSON 出力は [`schemas/diagnostics-v1.json`](schemas/diagnostics-v1.json)、baseline は [`schemas/baseline-v1.json`](schemas/baseline-v1.json) に従います。出力は決定的で、同じ入力に対して byte 単位で安定です。

## 既知の制限

- **対象バージョン。** 同梱の knowledge profile は Manim Community **0.20 のみ** を対象とします。他のバージョンのプロファイルはまだありません。
- **アセット検査は lint 実行マシンを調べます。** `MLR104` はリテラルなアセットパスを、lint を実行しているマシン上で Manim 自身のランタイム探索により解決します。プロジェクトツリー外の絶対パスについては、それは lint ホストに関する証拠であり、必ずしもレンダーホストのものではありません(例: CI で lint し、別マシンでレンダーするリポジトリ)。そのような診断は根拠として `environment_dependent: true` を持ちます。case-only の不一致は大文字小文字を区別する対象プラットフォーム(`linux`)に対してのみ報告されます。影響するプロファイルがすべて windows / macos を対象とする場合、宣言されたレンダーは書かれたとおりにファイルを解決できるため、linter は沈黙します。
- **ソースエンコーディング。** PEP 263 宣言は WHATWG ラベルと CPython コーデック別名テーブル(`latin-1`、`cp932`、`koi8_r`、...)で解決します。linter が表現できない稀な Python コーデックは、明示的な `MLC000` の「not supported by manim-lint」通知とともにスキップされます — 対象の Python がそのファイルをデコードできない、という主張には決してなりません。
- **duration はリテラルのみから導出します。** Manim の*デフォルト*に依存する play(`run_time` を一切書かない `self.play(m.animate.shift(RIGHT))`、`self.wait()`)の duration は unknown と報告され、フレーム数は数値の代わりに per-frame 表現になります(保守的: 欠落はしても捏造はしない)。リテラルな play レベルの `run_time` は play 全体の duration を正確に決定します — Scene ヘルパー内の play も呼び出しサイトごとに、呼び出しサイトが異なる(あるいは追跡不能な)mobject をパラメーター上の `.animate` ビルダーに渡す場合も含めて — 一方、*非リテラル*な `run_time`(や `**kwargs` splat)は、それが上書きするコンストラクタリテラルを正直に unknown へ広げます。
- **サマリー由来の play は保守的です。** ヘルパーのインライン化が effect summary へフォールバックした場合(再帰、解決不能な呼び出し — カバレッジレポートの `helper calls summarized, not inlined` に計上)、そのヘルパーの play は `Maybe` 確度・開いた繰り返し回数のレコードとして現れます: `MLC104` のようなリテラル duration 検査はそこでも発火しますが、呼び出し元の状態に依存する判定はすべて degrade されたままです。
- **`TracedPath` がコンストラクタで登録する updater はコスト専用です。** `TracedPath` が構築時に自身へ登録する updater はコスト目的ではモデル化されます(`MLP220` のスパン、lambda を渡した場合の hot context)が、ライフサイクル上の updater 登録ではありません: `TracedPath` 単独ではデフォルトの `wait()` はライフサイクルモデル上 dynamic にならず、バウンドメソッドの `traced_point_func` 本体は hot context として解析されません。
- **意図的に保守的な沈黙。** 一部の検出はカタログの記述より狭く、推測するより沈黙します。`MLR106` は NaN / inf をリテラル形式でのみ見て、`float("nan")` 呼び出しは追いません。`MLD301` は `dt` パラメータを持たない updater についてのみ FPS 依存を証明します(宣言だけして未使用の `dt` は指摘しません)。`MLC113`/`MLC124` はドキュメント化された呼び出し形のみを認識します。`MLR102` は play された裸の builder の target が不変であることを解釈器が証明できる必要があります。`MLR105` は検証済みの Pango サブセットを検査します(裸の `&` は許容)。`MLD304` は ThreeDScene の fixed-object cleanup 分岐のみを実装しています。各ルールの正確な範囲は `manim-lint explain <RULE>` が述べます。
- **未実装。** レンダー済みベースラインに対する閾値較正、nightly のレンダー比較 CI。

## 開発

```bash
cargo fmt --check
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

4 つのゲートすべてが通る必要があります。

knowledge profile のメンテナンス: `sync_manim_knowledge` バイナリは Manim のチェックアウトを静的に読み、レビュー可能なプロファイル候補を生成し、同梱プロファイルのドリフトを検査します(矛盾があれば exit 1)— [src/knowledge/profiles/README.md](src/knowledge/profiles/README.md) を参照してください。来歴(provenance)は分離されています: `upstream_0_20` は**クリーンな** upstream ベースコミット `4d25c031`(working tree ではなく `git archive` 経由で読む)を記述し、`local_0_20_1_4d25c031` オーバーレイは兄弟フォークの working tree がその上に追加するものを記述します:

```bash
# working tree(フォーク)— upstream に対しては情報提供のみ
cargo run --bin sync_manim_knowledge -- --manim-root ../manim --diff
# クリーンな upstream ベース — 矛盾ゼロでなければならない
cargo run --bin sync_manim_knowledge -- --manim-root ../manim --manim-ref 4d25c031 --diff
cargo test --test knowledge_drift -- --ignored   # layer-9 ドリフトゲート(両方)
```

### リリース品質ゲート(DESIGN §11.4)

リリースはさらに 3 つのゲートで守られます:

```bash
# ラベル付きコーパスゲート — `cargo test` の中で自動的に実行される。
# tests/corpus/manifest-v1.json が全コーパスケースの sha256 と
# 期待される診断の正確な内容(true positive と false-positive ガード)を
# ピン留めする。実 Manim example_scenes のスナップショットと
# 敵対的レビュープローブを含む。
cargo test --test corpus_gate

# ベンチマークゲート — 明示的に実行、release ビルド、静かなマシンで。
# ピン留めされた 10k-LOC フィクスチャ(tests/corpus/benchmark_10kloc)で
# cold ≤ 2 s / warm cache hit ≤ 0.5 s / peak RSS < 300 MiB。
# 閾値は benchmarks/reference-machine.json
# に一致するマシンでのみ assert され、それ以外では情報提供のみ。
# gateはcoldがcache miss、warmが検証済みhitであることも証明する。
# リファレンスマシンでの3回の中央値(2026-07-19): cold 0.409 s、
# warm 0.006 s、peak RSS 238.8 MiB — すべてバジェット内。
cargo test --release --test benchmark_gate -- --ignored benchmark

# knowledge ドリフトゲート — 兄弟の Manim チェックアウトが必要。CI では
# schedule / dispatch 時に、ピン留めされたベースコミットの shallow clone に
# 対して実行される。
cargo test --test knowledge_drift -- --ignored
```

コーパスケースを機械的に記録し直すことはありません: 不一致は [CONTRIBUTING.md](CONTRIBUTING.md#corpus-labeling) のラベリングプロトコルに基づく再判定(re-adjudication)を意味します。

リポジトリ構成、ルール追加の手順、すべての変更が守るべき不変条件は [CONTRIBUTING.md](CONTRIBUTING.md) を、パイプラインと事実レイヤーの概観は [docs/architecture.md](docs/architecture.md) を参照してください。`DESIGN.md` が正典であり、公開契約の変更は DESIGN.md・スキーマテスト・ルールドキュメントを同時に更新する必要があります。

## ライセンス

[MIT](LICENSE)。
