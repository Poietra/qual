# Qual 設計記録（historical design record）

> **この文書は仕様書ではない。実装前に書かれた設計記録である。**
>
> 本書は Rust 実装が存在しない時点で、Python 実装を前提に書かれた。内容は
> コードと同期しておらず、同期させる予定もない。実装を変更する前にこれを
> 読む必要はなく、本書と実装が食い違った場合は常に実装側が正しい。
>
> 現行の正典は次のとおり:
>
> | 知りたいこと | 正典 |
> | --- | --- |
> | 実装配置・fact layer・不変条件 | [`docs/architecture.md`](docs/architecture.md) |
> | Manim の意味モデル（旧 §3） | [`docs/architecture.md`](docs/architecture.md) の "The Manim semantic model" |
> | rule catalog（92 rules） | [`docs/rules/`](docs/rules/README.md) |
> | CLI・設定・JSON 契約 | [`docs/reference/`](docs/reference/cli.md)、[`schemas/`](schemas/) |
> | 作業手順・rule の追加方法 | [`CONTRIBUTING.md`](CONTRIBUTING.md) |
>
> **言語方針**: 正典となる文書は英語で書く（README、`docs/`、`CONTRIBUTING.md`）。
> 本書が日本語のみであることは、設計記録として保存する以上そのままにする。
> 英語話者の contributor に本書の読解を要求しない。
>
> 残した理由: §3 の意味モデルと §4 の記号的コストモデルの導出、および §15 の
> 不変条件がなぜその形なのかという判断過程は、今も価値がある。ただし
> §3 と §15 はすでに `docs/architecture.md` に英語で移してあり、そちらが正典である。

- 執筆時点の想定: Manim Community 0.20 系（実地参照は 2026-07-17 時点の `0.20.1`、基底コミット `4d25c031`）
- 執筆時点の想定実装言語: Python（実際の実装は Rust 2024 edition / rustc 1.85 以上）
- 実装状況: 完了。rule catalog は 92 実装 / 0 予約

## 1. 結論

`qual` は一般的な Python linter の薄いプラグインにはしない。対象コードを実行も import もせず、次の三つを同時に扱う独立した静的解析器として作る。

1. Manim のライフサイクルを追う抽象解釈器
2. Cairo / OpenGL の描画特性を含む、記号的なコスト推定器
3. Manim 固有の誤描画、実行時エラー、性能劣化を説明するルールエンジン

解析の中心は標準ライブラリの `ast` と `tokenize` にする。Ruff には外部ルールの安定したプラグイン API がなく、Flake8 の単一ノード visitor では複数文にまたがる Scene 所属や Animation cleanup を表現しにくい。LibCST にも Manim の CFG や状態解析はないため、MVP の中核には入れない。LibCST は複雑な構造的 autofix が本当に必要になった段階で fixer 専用の任意依存として追加する。

最大の設計原則は、API 名だけで警告しないことである。たとえば `FadeOut(mob)` は、`mob` が Scene に未追加でも play の準備段階で自動追加され、終了後に remover として削除される。反対に、1 引数 updater は通常の `wait()` を動的にする根拠にならない。このような実際の状態遷移を再現してから診断する。

## 2. 目的と非目的

### 2.1 目的

- レンダリング開始前に、確実な Manim 実行時エラーを検出する。
- 「例外は出ないが、意図した絵にならない」ライフサイクル上の誤りを検出する。
- updater、Animation、family、curve、pixel、renderer の乗数を使い、重い箇所と理由を示す。
- `from manim import *`、alias import、Scene helper、fluent chain、`.animate` を理解する。
- Cairo と OpenGL で意味またはコストが変わるコードを対象 profile ごとに診断する。
- 診断に根拠、確度、影響する profile、可能なら実行回数の上界を含める。
- CI、editor、既存プロジェクトへの段階導入に必要な JSON、SARIF、baseline、抑制を提供する。

### 2.2 非目的

- Python 全般の lint を再実装しない。未使用 import、一般的な型エラー、通常の closure 問題は Ruff / Pyright に任せる。ただし Manim の毎フレーム実行によって結果が誤描画になる場合は、Manim 固有の説明を付けて扱ってよい。
- linter 実行中に Scene を render しない。
- ユーザーモジュール、Manim、plugin を import しない。
- すべての動的 Python を完全に解決しない。分からない値は `Unknown` にし、推測を確定エラーとして出さない。
- 初版から壁時計ミリ秒を断言しない。静的な乗数と階級を基本にし、実測係数は任意の calibration profile に分離する。
- 低確度の「書き方の好み」を default では出さない。

## 3. Manim の意味モデル

### 3.1 Scene 全体のライフサイクル

実装は最低限、次の順序を意味として持つ。

```text
module import
  ↓
Scene.__init__ / renderer・camera・file writer 準備
  ↓
Scene.render()
  ├─ setup()
  ├─ construct()
  │    ├─ object construction / family mutation
  │    ├─ add / remove / foreground / fixed-in-frame
  │    └─ play / wait を 0 回以上
  ├─ tear_down()
  └─ renderer.scene_finished() / partial movie 結合・終了処理
```

`Scene.render` は `setup → construct → tear_down` をこの順で呼ぶ。`setup` と `tear_down` の基底実装は空だが、ユーザー定義の中間基底 Scene が処理を持つ場合は、override で `super()` を省いたことが意味を持つ。したがって「すべての setup override に super を要求」してはいけず、解決済み基底 method に効果がある場合だけ候補にする。

### 3.2 `Scene.play` の正確な状態遷移

通常の Cairo / OpenGL play を、解析器では次のイベント列として扱う。

```text
compile arguments
  ├─ Animation はそのまま
  ├─ mob.animate... は _AnimationBuilder → Animation
  └─ その他は実行時 TypeError
      ↓
apply play kwargs to animations
      ↓
auto-add non-introducer animation targets not in Scene family
      ↓
derive duration = max(animation.run_time)
      ↓
Wait の static / dynamic 判定
      ↓
animation._setup_scene(scene)
  └─ introducer は必要なら Scene.add
      ↓
animation.begin()
  ├─ starting_mobject の copy
  ├─ 通常は live mobject の updater を suspend
  ├─ Transform は target copy と align_data
  └─ interpolate(0)
      ↓
moving / static object scope の決定
      ↓
各 time-grid sample
  ├─ animation private objects の updater
  ├─ animation.interpolate(alpha)
  ├─ Scene mobjects の recursive updater
  ├─ meshes の updater
  ├─ Scene updater
  ├─ raster / readback / encode handoff
  └─ stop_condition
      ↓
animation.finish()
  ├─ interpolate(1)
  └─ suspended updater の resume
      ↓
animation.clean_up_from_scene(scene)
  ├─ remover は Scene.remove
  └─ ReplacementTransform は Scene.replace
      ↓
Scene.update_mobjects(0)
```

重要な含意:

- `Transform(mob, target)` の結果は通常 `mob` 自身が target の形になる。target を Scene に残すわけではない。
- `ReplacementTransform(source, target)` は cleanup で source を target に置換する。
- introducer / remover の Scene 所属効果は、Animation を構築した時点ではなく play の setup / cleanup で起きる。
- 同じ play で同じ live family を二つの Animation が書くと、後から実行された補間が前の結果を上書きし得る。
- Animation の live mobject updater は通常 suspend されるが、Scene 内の別 mobject、starting copy、target copy、Scene updater には別の規則がある。
- `.animate` は単なる遅延 syntax ではない。builder を取得した時点で `generate_target()` が走るため、builder 作成から play までに live object を変更したり、同じ object の別 builder を作ったりすると、stale / overwritten target を使う可能性がある。

### 3.3 frame time と updater

通常レンダリングの frame 時刻列は `np.arange(0, run_time, 1 / frame_rate)` 相当である。静的推定では概ね `ceil(run_time * fps)` と表示してよいが、境界の浮動小数点差を理由に「正確な frame 数」とは書かない。`finish()` の `alpha=1` は geometry の最終状態を作るが、通常は追加の動画 frame を書かない点も区別する。

1 frame 内の通常順序は次である。

```text
Animation.update_mobjects(dt)
Animation.interpolate(alpha)
Scene.update_mobjects(dt)   # top-level から submobjects へ再帰
Scene.update_meshes(dt)
Scene.update_self(dt)       # Scene updater は最後
Renderer.render(...)
```

Mobject updater の呼び出し規約には落とし穴がある。Manim は callback の signature に名前が `dt` の parameter があるかを調べ、あれば `(mobject, dt)`、なければ `(mobject)` で呼ぶ。単に「引数が二つ」であるだけでは time-based updater と認識されない。

`MLC105` はparameter数の独自近似ではなく、この分岐をそのまま模倣し、`inspect.Signature.bind` 相当で位置引数が受理されるか検証する。したがって `lambda dt:`、keyword-only `dt`、`lambda mob, delta:` を正しくエラーにし、default parameter、positional-only、`*args` は実際にbindできる限り許可する。`Scene.add_updater` は別契約で `(dt)` 一引数を常に渡す。

`wait()` の自動 freeze 判定は、次のいずれかがある場合に動的になる。

- `always_update_mobjects`
- Scene updater
- `stop_condition`
- Scene family 内の time-based updater

1 引数 updater しかない場合、default の `wait()` は静止画扱いになり得る。これは linter が明示的に扱うべき Manim 固有の誤描画候補である。

### 3.4 Scene 所属、family、描画順

- `Scene.add` は単なる set insert ではない。同じ object をいったん除外して末尾へ置き、描画順を変える。
- parent を Scene に加えると submobject も family として表示対象になる。
- `Mobject.add` は同じ child の重複を無視し、直接の自己追加を拒否する。
- `VMobject` family には通常 VMobject だけを入れ、異種の Mobject をまとめる場合は `Group` が必要である。
- `Scene.remove(child)` は parent の `submobjects` を編集しない。Scene 側の root list を分解して child 以外を残すだけなので、元の parent を後で再び `add` / animate すると child が再出現する。この temporary removal と structural removal を区別する。
- Cairo は z-order と source-over 合成の都合上、最初の moving / updater-bearing object 以降の suffix を再描画対象にしやすい。Scene リストの位置は性能にも影響する。
- foreground object は moving scope を広げ得る。
- OpenGL は retained render plan を使える場合と、custom subclass / callback により immediate fallback になる場合でコストが変わる。

### 3.5 3D と fixed object

`ThreeDScene.add_fixed_orientation_mobjects` と `add_fixed_in_frame_mobjects` は object を自動的に Scene へ追加する。現行実装では対応する remove API の所属効果が renderer で異なる箇所がある。Cairo は camera の固定登録だけを解除し、OpenGL branch は unfix 後に `Scene.remove` する。解除後も object が表示され続けると仮定したコードは renderer portability 診断の対象にする。

### 3.6 geometry storage は renderer 非依存ではない

public な `set_points_*` 系 API と raw `.points` 代入を同一視しない。現行 Cairo `VMobject` は cubic Bézier を 4 points/curve、OpenGL `OpenGLVMobject` は quadratic Bézier を 3 points/curve として扱う経路がある。したがって `points.reshape((-1, 4, 3))`、4 点単位の slice、Cairo の内部 layout を仮定した `set_points` は OpenGL portability bug になり得る。knowledge profile に renderer ごとの point layout を持ち、raw access は shape と対象 renderer が確定した場合だけ強く診断する。

## 4. 記号的コストモデル

### 4.1 Scene の総コスト

最初から一つの「推定 ms」に潰さない。次の stage 式を保持する。

```text
T_scene = T_import
        + T_setup_construct
        + Σ play (
              T_compile
            + T_hash
            + T_begin_copy_align
            + F × (
                  T_animation_update
                + T_interpolate
                + T_scene_family_update
                + T_display_list
                + T_raster_or_draw
                + T_readback_handoff
                + T_encode
              )
            + T_finish_cleanup
            + T_partial_stream
          )
        + T_concat_finalize
```

主な記号:

| 記号 | 意味 |
| --- | --- |
| `F` | play の rendered frame 数の区間 |
| `N_root` | Scene の top-level root 数 |
| `N_family` | 展開後の family member 数 |
| `N_moving_suffix` | Cairo が各 frame で再描画する suffix の規模 |
| `P` | point 数 |
| `C` | Bezier curve 数 |
| `S` | subpath 数 |
| `A_px` | device pixel coverage / frame pixel area |
| `D` | draw call / shader wrapper 数 |
| `L_tex` | distinct TeX compile job 数 |
| `K_resource` | frame 列を通した distinct Text / TeX / SVG / Image cache key 数 |
| `B_frame` | frame buffer bytes |

値は `Exact(n)`、`Interval(lo, hi)`、`Symbol(name)`、`Unknown` のいずれかで持つ。未知値を 1 と仮定して過小評価してはいけない。

通常の RGBA frame なら `B_frame = 4 × pixel_width × pixel_height`、pixel-bandwidth の基本乗数は `pixel_frames = F × pixel_width × pixel_height` と表せる。transparent / pixel format / readback path が違う profile では channel 数と追加 copy を OutputState から補正する。

### 4.2 invocation context と multiplicity

flat な frequency enum ではなく、すべての operation fact に「どの phase から呼ばれるか」と記号乗数を直交して持たせる。

```text
InvocationContext =
  MODULE_IMPORT | SCENE_INIT | SETUP | CONSTRUCT | PLAY_BEGIN |
  FRAME_CALLBACK | RASTER | PLAY_CLEANUP | TEAR_DOWN | UNKNOWN

Multiplicity =
  loop_iterations × plays × frames × roots × family × points ×
  curves × samples × pixels × distinct_resource_keys
```

external compiler / process launch は frequency ではなく `OperationKind.EXTERNAL_PROCESS` であり、その invocation に `distinct_resource_keys` 等の乗数を付ける。

例:

```text
MathTex construction is reachable from an updater.
production profile: 60 FPS × 8 s ≈ 480 invocations.
Each invocation constructs an object and checks the cache; an unseen key also compiles TeX.
A frame-varying expression may produce about 480 distinct disk assets.
```

duration が不明なら「per-frame」とだけ表示し、架空の 480 回を出さない。

per-frame context の入口は callback 本体だけでなく、その推移的 helper call に伝播する。初版で認識する入口は次である。

- `Mobject.add_updater` の callback
- `Scene.add_updater` の callback
- `always_redraw` の factory（初回 1 回に加えて毎 frame）
- `UpdateFromFunc` / `UpdateFromAlphaFunc`
- `Wait(stop_condition=...)`
- custom `Animation.interpolate` / `interpolate_mobject` / `interpolate_submobject`
- `TracedPath.traced_point_func`
- custom Scene / Mobject update hook

callback が project 内 helper を呼ぶ場合、その summary の invocation context を `FRAME_CALLBACK` とし、`frames` を掛ける。別の cold call site からも呼ばれる helper 自体を一律 hot とせず、call context ごとに context と multiplicity を持つ。

### 4.3 stage 別の支配項

#### Animation begin

- 基本 Animation は `starting_mobject = mobject.copy()` を作る。
- Transform は target copy、family alignment、point / subpath / curve 挿入を行う。
- topology が同じなら copy が主になり、違えば `align_data` / `insert_n_curves` が主になり得る。
- 現 fork の実測では代表的 Mobject tree の deepcopy 特殊化は約 `1.18x`。したがって copy は「無料になった」とみなさない。
- 500-member probe では topology 一致の begin が約 `11.436 ms` で copy が 64.6%、1→16 curve mismatch は約 `64.466 ms` で align が 81.5% だった。`N_family` だけでなく topology / curve-count delta を独立 dimension にする。

#### interpolation

- canonical path は frame ごと、Animation ごと、family member ごとに point と style を補間する。
- 現 fork の Cairo packed path の実測では 300 members / 60 frames の補間 stage が `130.658 ms/play → 33.004 ms/play`、steady-state が `2.0761 → 0.1890 ms/frame`。fast path の gate 外、custom animation、updater 付き family では canonical cost が残る。
- したがって `F × N_family × channels` を保持する。

#### updater / family walk

- `Scene.update_mobjects` は top-level root から submobjects を再帰する。
- updater 自身が軽くても、大きな updater-free tree の walk が frame ごとに発生し得る。
- 実測では 3,000-member family の update stage が fast authorization 前 `0.3665 ms/frame` 程度あった。小さな単発 cost でも `F` を掛ける。
- updater の `dt` parameter は存在するだけで plain `wait()` を動的化する。body が `dt` を使わず frame-invariant なら、幾何処理だけでなく raster path まで不要に開く可能性がある。

#### Text / TeX / SVG

- `Text` / `MarkupText` は shaping、SVG 生成・parse、glyph family 構築を含む。
- 1,000-character Text の glyph closure 改善だけで約 `20.9 ms` の end-to-end 差が観測されており、per-frame construction は明確に高コストである。
- `MathTex` / `Tex` は cache miss 時に外部 compiler と `dvisvgm` を伴う。distinct formula は `L_tex` 個の external process cost として扱う。
- 同じ key の `Text` は shaping / SVG生成を、同じ key の `MathTex` は compiler / `dvisvgm` を cache-hit で省ける。ただし毎回の object construction、cache lookup、通常の SVG parse / family 構築は残る。tracker値を埋め込んだ frame-varying expression は `K_resource ≈ F` となり、disk asset も frame 数近く増え得る。
- 現 fork の 16 distinct formulas では submit-all/collect が sequential の `120.7 s → 11.7 s` だった。複数の独立式を serial に作る箇所は profile-gated advice の価値が高い。
- `SVGMobject` は XML parse、path command 解釈、point/family 構築を行う。単なるファイル read とみなさない。

#### Cairo

- static base の作成は play 単位、moving suffix の raster と source-over 合成は frame 単位。
- curve / fill / stroke operation と `A_px`、antialias、透明 layer が支配する。
- list 前方の object に updater があると、後続の大きな static suffix まで再描画しやすい。
- full-frame reset / handoff / encode は geometry が少なくても残るため、軽い Scene で pixel area が相対的に支配する。
- frozen wait でも動画の duration を保つため writer / encoder 側は frame 数相当を処理する。`frozen_frame=True` は主に updater、補間、再rasterを省くのであり、動画長の encode cost がゼロになるという説明はしない。

#### OpenGL

- CPU geometry preparation、uniform / buffer synchronization、draw、FBO readback、writer handoff、encode を分ける。
- retained path と immediate fallback を区別する。
- asynchronous readback は解像度が小さいと overhead が逆転し得るため、「OpenGL なら常に速い」とは診断しない。
- text-heavy 2D と large 3D surface では適切な renderer が異なる。renderer recommendation は advisory とし、profile と workload evidence を必ず表示する。

#### 3D

- Surface は概ね face 数に比例して sort、projection、shading、raster が増える。
- 1,024-face Surface の現 fork 実測では、最適化前の z-sort stage が約 `17 ms/frame`、main projection が約 `6.4 ms/frame` 規模だった。最適化後も face × frame の乗数は残る。

### 4.4 severity の決め方

performance rule の severity は API 名で固定せず、multiplicity で上げる。

```text
score = operation_weight
      × frequency_weight
      × known_size_weight
      × profile_multiplier
```

- `MathTex()` が construct で一度: 原則診断しない。
- 同じ `MathTex()` key が固定 loopで反復: object/SVG再構築のinfo候補だが、compile jobは一件なのでparallel-precompileを勧めない。
- cold cacheで4件以上のdistinct `MathTex()` key: local capabilityがあるprofileだけserial-precompile advice。
- `MathTex()` が updater / `always_redraw` 内: warning 以上。
- `copy()` が小さな Dot に一度: 診断しない。
- `copy()` / `align_data()` が large family の updater 内: warning。

calibration がない場合の出力は「high multiplicity」「per-frame × family-linear」のような階級にする。将来 `qual calibrate` を追加する場合も、machine / Manim source digest / renderer / resolution を含む別 JSON に係数を保存する。

## 5. 解析アーキテクチャ

### 5.1 pipeline

```text
config/profile resolution
    ↓
SourceManager: read + ast.parse + tokenize
    ↓
ProjectIndex: modules/imports/symbols/class hierarchy
    ↓
qualified Manim call facts
    ├─ SemanticDependencyGraph: file/definition forward + reverse edges
    ↓
CFG + reachable method summaries
    ↓
Manim abstract interpreter
    ├─ lifecycle events
    ├─ renderer facts
    ├─ symbolic cost facts
    └─ SemanticDependencyGraph: Scene/play/object edges
    ↓
rule queries
    ↓
suppression + baseline
    ↓
text / JSON / SARIF / fixes
```

project 全体の処理順は固定する。

1. 対象全ファイルを parse する。
2. module / import graph を構築する。
3. symbol table と class hierarchy を確定する。
4. helper の strongly connected components ごとに method summary を固定点計算する。
5. 各 Scene の constructor path を解析し、MRO に沿って `__init__ → setup → construct → tear_down` の効果を合成する。
6. facts に対して rule を実行する。
7. `(relative path, line, column, rule_id)` で安定 sort する。

一つのファイルが `SyntaxError` でも残りは継続し、そのファイルには parser diagnostic を出す。

custom `__init__` は対象外にしない。Scene allocation が見える場合は Python MRO と `super()` chain に沿って effect summary を合成する。MVP は direct / project-local Scene・Mobject subclass と明示的 `super().__init__` を扱い、metaclass factory、dynamic base、cooperative multiple inheritance が解決不能なら constructor state を Unknown にする。`setup` も同じ MRO 規則で合成し、「基底が空だから常に super 不要」と仮定しない。

### 5.2 SourceManager

Python AST の `col_offset` / `end_col_offset` は UTF-8 byte offset である。日本語を含む Scene でも fix span を壊さないよう、次の変換を `SourceManager` 一箇所へ集約する。

- `(line, UTF-8 byte column) → absolute character offset`
- absolute character offset → `(line, display column)`
- AST span → source slice
- token / comment lookup
- newline style と source encoding の保持

fix 適用後は必ず再 parse し、失敗したら全 edit を rollback する。
（実装との差異: これは Python 実装を前提に `ast.parse(feature_version=...)` を
想定して書かれた。Rust 実装が同梱する grammar は rustpython-parser 0.4 の
Python 3.12 固定で、`feature_version` に相当する pin は持たない。`target-python`
は parse の仕方を変えず、parse された構文が target より新しい場合に `MLC000` として
報告する post-parse gate である。`src/reporting/fixes.rs` と
`src/frontend/features.rs` を参照。）

### 5.3 import と名前解決

最低限、次を同一の qualified symbol に解決する。

```python
from manim import *
from manim import Scene, Square as Sq
import manim
import manim as mn
from my_scenes.base import BaseScene
```

必要な index:

- module exports と `__all__`
- explicit / star / relative import edge
- local assignment と shadowing
- class inheritance
- function / method definition
- qualified call target の候補集合

解決不能な star import や dynamic `getattr` は関係値を `Unknown` にする。未知 call に Mobject を渡した場合はその geometry / style / updater / parent relation を、`self` を渡した場合は Scene membership / order / updater を、関係する closure/global を渡した場合は対応 heap fact を必要な範囲だけ widen する。widen 後の fact から high-confidence 診断を出さない。名前が `Scene` だからという文字列一致だけで Manim Scene と断定しない。

### 5.4 versioned Manim knowledge profile

linter 実行時に Manim を import しない。次を JSON profile として同梱する。

- public class と完全修飾名
- Scene / Mobject / VMobject / OpenGLMobject / Animation の継承関係
- method signature と return alias
- fluent mutator の `returns_self`
- Scene API の state effect
- Animation の target position、introducer、remover、suspend、replacement effect
- `.animate` / builder の意味
- renderer 固有 API と互換性条件
- operation の cost dimensions
- `Create` のような accepted kind 制約
- compatible version range / source digest / schema version

初版を `v0_20.json` とし、`tools/sync_manim_knowledge.py` が `../manim` を静的に読んで候補を生成する。生成物は人間が review して commit し、通常の lint 中には再生成しない。

profile は upstream public semantics と local fork overlay を分離できる形式にする。

```text
knowledge/profiles/v0_20.json
knowledge/profiles/local_0_20_1_4d25c031.json   # 必要な場合
```

最小 schema 例:

```json
{
  "schema_version": 1,
  "name": "manim-community-v0_20",
  "manim_version": ">=0.20,<0.21",
  "source_digest": "sha256:...",
  "base_profile": null,
  "symbols": {
    "manim.animation.creation.Create": {
      "kind": "animation",
      "accepted_target": "VMobject",
      "effects": {"introducer": true, "remover": false}
    }
  },
  "deleted_symbols": []
}
```

overlay は `base_profile` の name と digest を必須にし、qualified symbol 単位で entry 全体を置換する。削除は `deleted_symbols` に明示し、曖昧な recursive deep merge はしない。generator が安全に抽出できるのは主に signature、継承、定数であり、introducer / remover / write-set / callback frequency / renderer effect は curated data として review する。timestamp は生成物へ入れず、同じ source から byte-identical profile を作る。

known Manim class の `begin`、`interpolate`、`clean_up_from_scene`、Scene API 等を user subclass が override した場合、その method の curated effect は継承しない。project 内 override を解析して summary を作り、解決不能なら該当 effect を Unknown にする。instance method monkeypatch も同じく trust を無効化する。

Manim version が不明なら最新互換 profile を使えるが、version-sensitive rule の confidence を下げ、出力にその事実を含める。

### 5.5 abstract values と heap

仮想 Object ID は `(allocation site, bounded call context, cardinality)` で作る。同じ helper を二回呼ぶ場合や loop 内 allocation を一つの identity に潰さない。cardinality は `singleton | many | maybe-many` を持ち、`many` 同士を同一 identity と断定しない。

代入、return、container、`returns_self` と分かる fluent mutator は同一 identity を伝播する。`copy` / `deepcopy` / `generate_target` / Animation starting copy / target copy は必ず新しい Object ID とし、`copy_of` relation だけを記録する。

```python
Presence = ABSENT | PRESENT | MAYBE
Truth = NO | YES | MAYBE
Visibility = VISIBLE | INVISIBLE | MAYBE
Confidence = CERTAIN | HIGH | MEDIUM | LOW
```

`MobjectState`:

- kind 候補集合
- allocation site と alias provenance
- top-level Scene root membership
- family 経由の effective Scene membership
- structural parent / submobject relation
- visibility と points / fill / stroke opacity facts
- foreground / fixed-orientation / fixed-in-frame
- updater 集合、signature、time-based 判定
- updating suspended
- family / point / curve / subpath size interval
- mutation epoch
- generated target
- renderer compatibility

`AnimationState`:

- kind
- live target と関連 family
- introducer / remover / replacement
- suspend behavior
- `run_time` interval
- 同時 play group
- topology change の可能性
- write set / read set

`SceneState`:

- Scene / renderer kind
- mobject order と membership
- foreground / mesh collections
- elapsed time interval
- current phase
- current invocation context と symbolic multiplicity
- active updater と animation

`CameraState`:

- MovingCamera frame / ThreeDCamera tracker
- ambient camera updater と camera motion interval
- fixed-orientation / fixed-in-frame registrations
- camera motion が Scene 全体を moving にする区間

`OutputState`:

- pixel width / height / frame rate
- `cairo_static_layers` / `cairo_fork_workers`
- video encoder と continuous / per-play writer mode
- transparent / sound / sections
- antialias と OpenGL readback mode

`ResourceState`:

- known / symbolic Text・TeX・SVG・Image key
- distinct key count `K_resource`
- cache assumption `cold | warm | unknown`
- submitted / collected TeX Future（local fork profile）

分岐 join では異なる状態を `MAYBE` にする。通常 mode では `MAYBE` だけを根拠に error を出さず、likely / advisory に落とす。loop は 0 回と 1 回を評価し、最大 3 iteration で固定点、収束しなければ interval と heap relation を widen する。

### 5.6 event IR

rule が AST を個別に歩く構造にしない。interpreter が次のような event / fact を生成する。

```text
Alloc(kind, object_id)
Alias(dst, src)
ReturnAlias(self | parameter | allocation)
SceneAdd(objects, order_effect)
SceneRemove(objects)
AddChild(parent, child)
RegisterUpdater(target, callback, signature)
SuspendUpdater(target)
ResumeUpdater(target)
Mutate(target, mutation_kind)
CreateAnimation(kind, targets, effects)
BeginPlay(animations, duration)
FrameCallback(call, invocation_context, multiplicity)
FinishPlay(cleanup_effects)
RendererRequirement(kind)
Cost(operation, dimensions, invocation_context, multiplicity)
UnknownMutation(values)
```

この event 層が lifecycle rule と cost rule の共通の事実になる。

### 5.7 CFG と interprocedural summary

初版 CFG は次を扱う。

- `if`
- `for` / `while`
- `break` / `continue`
- `return`
- `try` / `finally`
- `with`
- `match`
- 内包表記は bounded summary

Scene から到達可能な project 内 helper について、次の effect summary を固定点計算する。

```text
Alloc
ReturnAlias
SceneAdd / SceneRemove
AddChild
RegisterUpdater
Mutate
CreateAnimation
Cost
UnknownMutation
```

recursive SCC が収束しない場合は、その summary 全体ではなく不明な effect だけを widen する。初版では project 内関数と known Manim API だけ interprocedural にし、third-party code は signature / configured stub がなければ Unknown とする。

## 6. 診断と rule API

### 6.1 ID taxonomy

| prefix | 分野 |
| --- | --- |
| `MLC` | lifecycle / definite correctness |
| `MLR` | rendering / geometry / renderer compatibility |
| `MLP` | performance / cost multiplicity |
| `MLD` | determinism / portability / cache stability |

ID は公開 API として一度 release したら意味を変えない。rule の細分化は新 ID で行う。

### 6.2 severity と confidence

二つを分離する。

```text
severity: error | warning | info
confidence: certain | high | medium | low
```

例: 同一 Mobject への同時 Animation は大きな visual bug だが alias が分岐由来なら `warning / medium`。literal negative duration は `error / certain`。

### 6.3 model

```python
Diagnostic(
    rule_id,
    severity,
    confidence,
    primary_span,
    message,
    explanation,
    related_locations,
    evidence,
    estimated_cost,
    applicable_profiles,
    fix,
)
```

`evidence` は機械可読にする。

```json
{
  "invocation_context": "frame-callback",
  "multiplicity": ["frames", "family"],
  "frames": {"lower": 480, "upper": 480},
  "family_size": {"lower": 46, "upper": null},
  "renderer": ["cairo"],
  "state_path": ["construct", "play#4", "updater:17"]
}
```

`RuleMetadata` は曖昧な slash 値を持たず、最低限次を固定する。

```python
RuleMetadata(
    id="MLC101",
    summary="Scene.play requires at least one animation",
    default_enabled=True,
    default_severity="error",
    minimum_confidence="certain",
    implementation_phase=1,
    required_profiles=(),
    required_capabilities=("qualified-calls",),
    supersedes=(),
)
```

個々の `Diagnostic.confidence` は evidence に応じて metadata の minimum 以上で変えられるが、default severity は一値である。条件により error と warning の意味が異なるなら rule ID を分ける。

rule interface:

```python
class Rule(Protocol):
    metadata: RuleMetadata

    def run(self, context: RuleContext) -> Iterable[Diagnostic]:
        ...
```

各 rule が独自 visitor を持たず、`RuleContext` の qualified calls、lifecycle transitions、membership、animation groups、hot contexts、renderer facts、cost estimates を query する。

fix は非重複 `TextEdit` の集合で、`SAFE` / `UNSAFE` を分ける。通常の `--fix` は SAFE だけを適用する。

JSON v1 の外部 envelope:

```json
{
  "schema_version": 1,
  "tool_version": "0.1.0",
  "project_root": ".",
  "profiles": ["production"],
  "diagnostics": [
    {
      "rule_id": "MLC101",
      "severity": "error",
      "confidence": "certain",
      "path": "scenes/demo.py",
      "span": {
        "start": {"line": 12, "column": 8},
        "end": {"line": 12, "column": 19}
      },
      "message": "Scene.play() has no animations",
      "applicable_profiles": ["production"],
      "evidence": {},
      "fix": null
    }
  ]
}
```

`path` は project-relative POSIX 表記、line / column は 1-based Unicode character column、end は exclusive とする。不明な optional field は `null`、空集合は `[]` とし、byte column は外部 JSON へ出さない。

## 7. 初期 rule catalog

> **superseded.** 以下は執筆時点で構想した catalog であり、出荷された 92 rules と
> 一対一では対応しない（例: `MLC001` は本表に無いが実装されている）。ID ごとの
> 正典は [`docs/rules/`](docs/rules/README.md) と `src/rules/registry.rs` である。

以下は ID を先に予約する。`minimum confidence` は rule が発火できる最低確度であり、default 表示は設定の `min-confidence` に従う。

### 7.1 lifecycle / correctness

| ID | 検出内容 | default severity | minimum confidence | fix |
| --- | --- | --- | --- | --- |
| `MLC000` | 設定した target Python で source を decode / tokenize / parse できない。残りの file は解析継続 | error | certain | なし |
| `MLC101` | 引数なしの `Scene.play()` | error | certain | なし |
| `MLC102` | `play(mob.shift(...))`、bare Mobject、数値など Animation に変換できない引数 | error | high | `.animate` 化は unsafe suggestion |
| `MLC103` | 旧 API の `play(mob.shift, RIGHT)` のような bound method 渡し | error | certain | pattern が単純なら unsafe |
| `MLC104` | literal の `run_time` / `wait` duration が非正、または play 全体が 0 | error | certain | なし |
| `MLC105` | Mobject / Scene updater callback が Manim の実際の位置引数呼出へ bind できない | error | high | inline lambda 以外の rename は unsafe |
| `MLC106` | `stop_condition` と `frozen_frame=True` の併用 | error | certain | `frozen_frame=False` は unsafe |
| `MLC107` | `MoveToTarget(mob)` までの全 path で `mob.generate_target()` がない | error | high | なし |
| `MLC108` | 同じ live Mobject / overlapping family の同じ write channel を一つの play で複数 Animation が書く | warning | high | `.animate` chain への統合を提案 |
| `MLC109` | 空の `AnimationGroup` / `Succession` | error | certain | なし |
| `MLC110` | `Mobject.add(self)` または明白な parent cycle | error | certain | なし |
| `MLC111` | updater 付き object が Scene family / Animation owner のどちらにも入らない区間 | info | medium | `self.add(...)` は unsafe |
| `MLC112` | 1 引数 updater が frame-varying state を読むと証明できるのに、その updater だけを根拠として default `wait()` を使う | warning | high | `frozen_frame=False` suggestion |
| `MLC113` | `.animate` の animation kwargs を method access 後に渡す | error | certain | kwargs を前へ移動できれば safe |
| `MLC114` | override animation を含む unsupported `.animate` method chain | error | high | 別 Animation への分割を提案 |
| `MLC115` | `Scene.remove(child)` 後に元 parent を再追加・animateし、child が意図せず再出現 | warning | high | parent からも `remove` する提案 |
| `MLC116` | normal `Transform(source, target)` 後の source / target identity と Scene membership の差を後続操作が取り違える狭いpattern | info | medium | source alias を使う提案 |
| `MLC117` | `.animate` builder 作成後、play 前に同じ Mobject を変更・別 builder でtargetを上書き | warning | high | builder を play 式の直前へ移す提案 |
| `MLC118` | active updater 付き Mobject を通常 Animation の target にし、suspend区間とresume直後の状態差を客観的に示せる | info | medium | tracker animation または明示 option を提案 |
| `MLC119` | direct `Scene.replace(old, new)` で old が確実に Scene family 外 | error | high | なし |
| `MLC120` | `Restore(mob)` までの全 path で `save_state()` がない | error | high | なし |
| `MLC121` | updater / interpolate callback から `Scene.play` / `wait` / `pause` / `render` へ再入 | error | high | timeline操作をconstruct側へ移す提案 |
| `MLC122` | `ApplyMethod(mob.shift(...))` のように bound method でなく実行結果を渡す | error | high | methodと引数へ分離するunsafe fix |
| `MLC123` | inline `ApplyFunction` callback が全 path で Mobject を返さない | error | high | return追加はunsafe |
| `MLC124` | `.animate.get_*()` / `.animate.copy()` 等、戻り値だけを返してtargetを変異しないmethod | warning | high | 適切なmutatorを提案 |
| `MLC125` | `remove_updater(lambda ...)` 等、登録時と別のfunction identityを渡す | warning | high | callbackを変数へ保持する提案 |
| `MLC126` | 非 Mobject child、または VGroup / VMobject family への非 VMobject child | error | high | `Group` 候補を提案 |
| `MLC127` | 同一 child を一回の `add` に重複指定。Manimはwarningして無視 | info | certain | 重複引数の除去はsafe |
| `MLC128` | direct/project-local Scene・Mobject subclass の `__init__` が全 path で必要な `super().__init__()` を呼ばない | error | high | MRO不明時は発火しない |
| `MLC129` | `play(..., lag_ratio=x)` を複数Animation間staggerとして使う、または構築済みAnimationGroupのtiming変更を期待 | warning | medium | Group constructor側へ移す提案 |

`MLC108` は identity / family と write channel が確定した場合だけ出す。channel は少なくとも points、style、opacity、membership、camera state を分け、互いに素なcustom animationを同一ownerというだけで拒否しない。次は両方が通常Transformとしてpoints/styleを書き、確実な候補である。

```python
self.play(square.animate.shift(RIGHT), square.animate.rotate(PI))
```

望む意味が両方なら、多くの場合は次である。

```python
self.play(square.animate.shift(RIGHT).rotate(PI))
```

ただし rate function や path が変わる可能性があるため、自動 fix は unsafe とする。

`MLC114` は `@override_animate` が versioned knowledge、または alias 解決済みの project-local decorator から正に確認でき、同じ builder に二つ以上の method がある場合だけ出す。override が通常 method の後に現れる場合は override method access、override が最初の場合はその次の method access が `NotImplementedError` の位置になる。decorator または target kind が Unknown なら発火しない。

`MLC116` の初期実装は、全 path で完了した normal `Transform(source, target)` の後、source が Scene family に `PRESENT`、target が `ABSENT` の時点で、target を live target とする non-introducer `.animate` を play する狭い pattern に限定する。このとき play の auto-add により target が第二の object として追加される。`ReplacementTransform`、既に target が Scene にある場合、branch-only Transform は対象外にする。

`MLC118` は通常 Animation が live target updater を確実に suspend し、play 前に updater が確実に active で、その callback の全 path の write channel が完全に分類でき、Animation の write channel と重なる場合だけ出す。根拠は `begin()` の suspend、`finish()` の `alpha=1` と resume、その直後の `Scene.update_mobjects(0)` である。conditional / unknown callback、remove 済み updater、`suspend_mobject_updating=False`、互いに素な channel は発火させない。

### 7.2 rendering / renderer

| ID | 検出内容 | default severity | minimum confidence | fix |
| --- | --- | --- | --- | --- |
| `MLR101` | `Create` / `Uncreate` / `Write` / `DrawBorderThenFill` に非 VMobject | error | high | なし |
| `MLR102` | method call のない bare `mob.animate` を play し、target が不変 | warning | high | 削除は unsafe |
| `MLR103` | `MathTex("\\frac...")` 等、非 raw literal の Python escape が TeX を破壊 | error | high | raw prefix 追加は unsafe |
| `MLR104` | literal asset path が解決不能、case-only mismatch、`SVGMobject()` の file 欠落 | error | high | case correction は safe |
| `MLR105` | literal `MarkupText` の明白な tag nesting / entity エラー | error | high | 単純 closing tag は unsafe |
| `MLR106` | literal geometry に NaN / inf が流入 | error | high | なし |
| `MLR107` | 対象 renderer で未対応または意味が異なる API / mobject 組合せ | warning | high | なし |
| `MLR108` | fixed object の解除後も表示継続を仮定する renderer-divergent path | warning | high | 明示的 `add` / `remove` を提案 |
| `MLR109` | custom updater の read-after-write 順序により 1 frame lag が確定 | warning | medium | Scene add order または一つの updater へ統合 |
| `MLR110` | literal TeX の brace / environment 不整合を保守的 parser で確定 | error | high | なし |
| `MLR111` | Scene updater が Mobject を変更し、Cairo moving scope から漏れる可能性がある | warning | high | Mobject updater への移動を提案 |
| `MLR112` | raw `.points` / `set_points` が Cairo の 4-point cubic layout または OpenGL の 3-point quadratic layoutを固定仮定 | warning | high | public path API を提案 |
| `MLR113` | `Transform(mob, mob)` または source/target が確実な同一alias | info | high | なし |
| `MLR114` | literal / inferred points array が `N×3` でない、または対象rendererのcurve単位に不整合 | error | high | なし |
| `MLR115` | literal `font_size <= 0` | error | certain | なし |
| `MLR116` | 空pathへの `add_line_to` / `close_path`、または不完全curveのまま描画 | error | high | `start_new_path` を提案 |
| `MLR117` | `register_font(path)` context managerを `with` せず裸で呼ぶ | error | high | `with` 化はunsafe |
| `MLR118` | project内literal SVGのunsupported `<text>` / `<image>` / filter / mask / clipPath / unresolved href | warning | high | asset変換を提案 |
| `MLR119` | OpenGLを対象に含む `MovingCameraScene` / `self.camera.frame` の非互換path | error | high | Cairo限定profileを提案 |
| `MLR120` | OpenGL profileで `focal_distance` setterが有効だと仮定 | warning | high | renderer guardを提案 |
| `MLR121` | 2D Cairo Sceneで重なり順のためだけに `shift(OUT)` / `set_z` | warning | high | `set_z_index` を提案 |
| `MLR122` | `bring_to_front(mob)` 後も mob の `z_index` が他objectより低く、re-add順が無効 | warning | high | z_index修正を提案 |
| `MLR123` | `Object3D` / meshをCairoまたはrenderer不明profileでSceneへ追加 | error | high | OpenGL profileを提案 |
| `MLR124` | markupを期待して `Text("<b>…</b>")`、またはstatic-invalid `MarkupText` | warning | high | class変更はunsafe |
| `MLR125` | child / drawable pointsを持たないbare `Mobject()` leafを表示対象へ追加 | info | high | container用途なら抑制 |
| `MLR126` | literal opacityが `[0,1]` 外、またはnegative stroke width | error | high | なし |
| `MLR127` | literal `get_part_by_tex` / `set_color_by_tex` keyがknown MathTex分割単位に存在しない | warning | high | isolate指定を提案 |

`MLR103` は AST の復号済み string だけでは検査できない。`tokenize` から元 literal prefix と escape を読む。まず `\f` in `\frac`、`\a` in `\alpha`、`\b` in `\begin`、`\t` 系など、既知 TeX command と衝突する場合に限定する。意図的な newline を一律に禁止しない。raw prefix は runtime string を変更し、`\u` / `\N` / quote / 末尾backslashにも影響するので、初版では必ず UNSAFE suggestion とする。

`MLR105` は一般XML parserの受理/拒否をPango markupと同一視せず、Manim/Pangoと一致を検証したliteral subsetだけ high にする。unsupported construct は Unknown。`MLR110` も TeX macro、comment、verbatim、custom environmentを含む場合はbrace countingだけでerrorにせず、対応できるliteral subsetに限定する。

asset resolver はまず Manim runtime と同じ探索だけで validity を決める。概ね、profile の render `working-directory` から `Path(file_name)` を直接解決し、その後 `assets_dir / name{extension}` を Manim と同じ拡張子順で試す。source file directory や project root を勝手に runtime search path へ加えない。追加 root で候補を見つけた場合は「ここには存在するが実際の render 探索では見つからない」という修正候補として別 evidence にする。Windows absolute path は Linux `Path` として解釈せず、profile の `platform` と Windows path parser を使う。case-only mismatch も対象 platform ごとに示す。

`MLR109` の初期実装は exact な Manim kind を持つ leaf の top-level root 二つに限定する。reader は lambda updater 内の curated geometry write の直接引数として `driver.get_center()` を読み、writer は同じ points channel を全 path で書き、`dt` または確定した frame-varying state を読む必要がある。正の duration を持つ dynamic wait の直前 snapshot で両 updater が active、両 lexical binding が singleton identity に解決し、Scene root order が reader → writer と exact な場合だけ medium で出す。project subclass、writer-first、group recursion、branch order、複合式、named/unresolved callback は Unknown として発火しない。

`MLR118` は全 active profile が同じ canonical project-local file に literal `SVGMobject` path を解決する場合だけ asset を読む。静的 scanner が well-nested UTF-8 XML、quoted attribute、comment / CDATA / processing instruction を完全に消費できたときだけ、`<text>` / `<image>` / `<filter>` / `<mask>` / `<clipPath>` と、存在しない local `id` を指す plain `href="#..."` を high で報告する。DOCTYPE、malformed XML、entity 経由の id/href、external reference、profile ごとに異なる file、project 外 symlink は Unknown として黙る。analyzed Python や Manim は決して import / execute しない。

`MLR122` は Cairo profile が active で、`bring_to_front(singleton)` が既存 root を確実に末尾へ re-add した直後だけ検査する。root order、target と比較 object の scene membership、leaf/non-empty path、両 `z_index` が exact で、他の non-empty leaf の `z_index` が target より真に大きい場合だけ high で出す。同値なら stable sort の re-add tie order が有効なので発火しない。group、branch、Unknown mutation、OpenGL-only run も発火しない。

### 7.3 performance

| ID | 検出内容 | default severity | minimum confidence | cost evidence |
| --- | --- | --- | --- | --- |
| `MLP201` | updater / `always_redraw` / custom interpolate 内で `Text`、`MathTex`、`Tex`、`SVGMobject`、`Axes.plot`、`Surface` 等を構築 | warning | high | construction × frames |
| `MLP202` | hot context 内の `copy` / `deepcopy` / `become` / `align_data` | warning | high | family/points × frames |
| `MLP203` | hot context 内の `get_family`、`family_members_with_points`、`get_all_points`、bounding / arc-length / point-proportion query | info | high | family/curves × frames |
| `MLP204` | updater 内で `Scene.add/remove`、foreground 操作、毎 frame 新規 object を蓄積 | warning | high | graph mutation/allocation × frames |
| `MLP205` | `wait(frozen_frame=False)` の連続frameが同一と証明でき、未知callbackなし、duration/pixel閾値超過 | warning | high | full render × frames |
| `MLP206` | duration が profile の 1 frame 未満で clamp される | warning | certain | play startup for one frame |
| `MLP207` | topology / family / curve 数が大きく違う Transform | info | medium | align/curve insertion at begin |
| `MLP208` | large Text / MathTex family の Transform | info | high | copy + align + family interpolation |
| `MLP209` | Cairo effective display orderの前方にある animated/updater objectが大きなstatic suffixを毎frame invalidation | info | medium | moving suffix × frames |
| `MLP210` | 固定回数 loop 内の多数の短い逐次 `play` | info | medium | hash/begin/partial-stream × plays |
| `MLP211` | hot context 内の大きな list / ndarray / points の毎 frame allocation | info | medium | bytes allocated × frames |
| `MLP212` | full-screen 半透明 object / layer の長時間 animation | info | medium | pixel coverage × frames |
| `MLP213` | Cairoのlarge 3D Surface等、profileで較正済みのworkload/renderer mismatch | info | medium | renderer-specific dimensions |
| `MLP214` | 複数 distinct `MathTex` を serial construct。local overlayでprecompile/submit-allが可能 | info | high | distinct external jobs |
| `MLP215` | body が明白に no-op の updater が対象playのmoving scope、またはdt/Scene updaterとしてdynamic waitを広げる | warning | high | suffix/family × frames |
| `MLP216` | `always_redraw` が stable geometry への affine/style mutationだけで再構築を行う | info | medium | construction + become × frames |
| `MLP217` | hot callback 内で可変 key の `use_svg_cache=True` を使い、global cache を frame ごとに成長 | warning | high | memory `O(F × family)` |
| `MLP218` | `dt`未使用かつframe-varying readがなくidempotentと証明したupdaterがplain waitを動的化 | info | high | full frame pipeline × wait frames |
| `MLP219` | updater の推定生存期間が多数の後続 play に及ぶ | info | medium | active lifetime frames |
| `MLP220` | 長時間の `TracedPath(dissipating_time=None)` | warning | high | points `O(F)`、累積 raster `O(F²)` |
| `MLP221` | `ParametricFunction` / plot の literal step から過大 sample 数が確定 | warning | high | Python calls / points、hotなら × frames |
| `MLP222` | moving `ImageMobject` または Cairo moving suffix に巻き込まれた大画像 | warning | high | image area + full frame pixels × frames |
| `MLP223` | 透明なままの stroke に正の width が残り、stroke path を毎 frame処理 | info | high | curves × frames |
| `MLP224` | 長い path への `point_from_proportion` / general `apply_function` を hot callback で反復 | info | high | curve samples / points × frames |
| `MLP225` | opt-in cost report で、current fork の fork/static/bulk fast path を塞ぐ Scene updater、foreground、custom callback/path/rate、mesh等を説明 | info | high | lost parallel/static/packed path |
| `MLP226` | hot callback 内の frame-varying `Text` / `MathTex` / `Tex` key | warning | high | construction × frames + distinct disk assets `O(K_resource)` |
| `MLP227` | `always_update_mobjects=True` だが対象区間に time-dependent updater、Scene updater、stop condition、camera motion がない | warning | high | dynamic wait frames × full frame pipeline |

`MLP201` の例:

```python
label = always_redraw(lambda: MathTex(f"x={tracker.get_value():.2f}"))
```

これは値の更新だけなら `DecimalNumber` を一度作り、updater で `set_value` する方が適切なことが多い。診断は代替 API まで示すが、見た目と typography が変わるので自動 fix はしない。

`MLP209` は Cairo の描画順を変える提案が見た目を変え得るため advisory である。診断例:

```text
MLP209: updater-bearing background enters Cairo's effective z-ordered display list at position 1/84.
Approximately 83 later family members may be rasterized for every frame of this play.
If z-order permits, move the dynamic object later or separate the static layer.
```

`MLP204` は fresh allocation の有無を区別する。同じ object を `Scene.add` し直すだけなら主に reorder / plan invalidation だが、updater 内で `self.add(Dot())` や `group.add(Line())` を行うと family は `O(F)` に成長し、全 frame を通した描画量は典型的に `O(F²)` になる。

`MLP220` は `TracedPath` 自体を禁止しない。無制限に履歴を残すことが意図なら正しい表現であるため、duration、FPS、推定 points、`dissipating_time` の有無を evidence に含める。

`MLP221` は literal `t_range=(start, end, step)` から sample 数を区間評価する。NumPy ufunc だけからなる関数なら `use_vectorized=True` を候補にできるが、任意 Python callback を自動で vectorized 化してはいけない。

`MLP225` は通常の warning にしない。Scene updater、foreground、custom Animation、custom rate function、non-straight path、mesh、sound、section、transparent output などは正しい表現であり得る。`cost` report に「この feature のため fork-per-play は serial fallback」「この updater のため packed interpolation gate 外」のような因果だけを出し、意味を変える削除を勧めない。

`MLP225` の `RuleMetadata.default_enabled` は `false`、`required_capabilities` は `("cost-report", "local-fork-overlay")` とする。通常の `check` では発火させず、`cost` command か明示的な opt-in でだけ評価する。

fork の loss は `cairo_fork_workers >= 2` の profile だけで報告する。workers 0 は blocker ではなく未要求である。current fork では最初の unsupported play が親 encoder を開くと後続の eligible play も fork 不能になり得るため、playごとに独立な可否と仮定せず、OutputState に renderer-wide の単調な無効化を記録する。allowlist / blocker は versioned local knowledge から読み、`custom callback` という名前だけで一律拒否しない。

`MLP214` の precompile 提案は、local fork profile に API があり、cold cache で、最初の利用前に 4 件以上の distinct compile key が逐次構築される場合だけにする。同一式を何回作っても独立 compile job は一件である。fork 有効時は最初の play までに全 Future を collect できる範囲に限り、汎用 autofix は行わない。in-flight TeX worker は fork を serial fallback させ得る。

`MLP226` の診断例:

```text
Each invocation performs object construction and a cache-key lookup.
This expression depends on a ValueTracker and may create about 480 distinct keys
at 60 FPS for 8 seconds, including distinct Text/TeX disk assets.
```

performance rule は次の emission gate と重複排除を持つ。

- `MLP202`: large family / points が確定した場合だけ。`always_redraw` 内の暗黙 `become` は `MLP201` / `MLP216` と二重報告しない。
- `MLP203`: 同じ callback 内で mutation を挟まない重複 query、または既知の large family / long path に限定する。小さい Dot への一回の `next_to` / `get_center` は出さない。`point_from_proportion` は `MLP224` を優先する。
- `MLP204`: fresh object を persistent Scene / family へ加える成長系を warning/high とし、既存 object の reorder は大きな Scene だけ info。
- `MLP207` / `MLP208`: `N_family >= 32` または推定 curve insertion `>= 256` 程度から開始し、両方なら specialized な `MLP208` 一件にする。
- `MLP211`: small coordinate vector は除外し、静的に `>= 64 KiB/frame`、family loop 内、または既知 large points allocation に限定する。
- `MLP212`: exact curated `FullScreenRectangle`、constructor の `aspect_ratio` override / splat 不在、全 active profile が inherited default と同じ exact 16:9、play 直前の literal-derived `0 < fill_opacity < 1`、active updater 不在、style/opacity を書かない complete channel の certain direct-target animation、duration 下限 `>= 5 s` の全条件を要求する。profile ごとの frame pixel と pixel-frame 積を evidence にし、coverage / opacity / duration / target identity のどれかが Unknown なら出さない。
- `MLP213`: active Cairo profile、exact curated `Surface`、literal `resolution` から `u × v >= 1024 faces`、certain direct-target animation を要求する。`1024` は `docs/research/perf-evidence.md` の versioned `(32, 32)` calibration workload に結び付け、計測 machine の milliseconds は portable な診断値にしない。OpenGL-only、unknown / starred / smaller resolution、branch-only play は出さない。
- `MLP215`: 1 引数 no-op updater は普通の wait を dynamic にしない。通常 play の moving suffix と、`dt` / Scene updater による dynamic wait を別 evidence にする。
- `MLP216`: relative affine 更新への置換は累積 drift の可能性があるため info/medium。topology と absolute transform の同値性を証明できた場合だけ強める。
- `MLP218`: `dt` 未使用だけでは出さない。random、wall clock、external state にも依存せず、frame-invariant と証明できる時だけ high。
- `MLP219`: 用途終了を証明できなければ「不要」と断定せず、推定生存 frame 数だけを示す。
- `MLP222`: Cairo かつ image / screen area が閾値以上の場合。OpenGL に Cairo の PIL/full-frame cost を転用しない。
- `MLP223`: Cairo、singleton identity の certain direct-target play、non-empty path、exact `stroke_opacity == 0`、exact positive `stroke_width`、active updater 不在、current/future の style/opacity/unknown mutation 不在を要求する。透明 stroke が後で可視化される、または mutation / target / channel が Unknown なら出さない。
- `MLP209`: `cairo_static_layers=True` の standard Scene で後続 static run を保持できるなら severity を下げる。
- `MLP210`: continuous writer では per-play partial stream 境界が消えるため、OutputState が実際に per-play stream を示す時だけその cost を含める。
- `MLP213`: large Cairo 3D Surface は upstream `v0_20` profile と versioned calibration evidence で扱う。OpenGL text-heavy 2D の劣位は driver依存なので、calibration で確認済みの profile にだけ出す。

`MLP209` の位置は root追加順ではなく、family flatten、`z_index` のstable sort、foregroundを反映したCairo effective display orderから計算する。順序がUnknownなら「83個」のような定量値を出さない。

specificity の優先関係を `RuleMetadata.supersedes` で表す。（実装との差異: 出荷されている関係は `MLP224 > MLP203`、`MLP226 > MLP201`、
`MLP220 > MLP204/MLP211`、`MLP208 > MLP207`、`MLR119 > MLR107`、
`MLD305 > MLR104`。構想した `MLR112 > generic portability` は存在しない。
正典は `src/rules/registry.rs` の `RuleMetadata::supersedes`。）同じprimary span・同じ根拠なら最も具体的な一件だけを出す。

`MLP214` と `MLP225`、`cairo_fork_workers` / `cairo_static_layers` のfast-path解釈は local fork overlay専用であり、upstream `v0_20` では無効にする。`MLP217` も knowledge profile が同じprocess-global SVG cache semanticsを宣言する場合だけ有効化する。存在しないAPIや設定を提案しない。

### 7.4 determinism / portability

| ID | 検出内容 | default severity | minimum confidence |
| --- | --- | --- | --- |
| `MLD301` | updater が `shift` / `rotate` / `increment_value` を frame ごとに行うが `dt` で scale されず FPS 依存 | warning | high |
| `MLD302` | updater / interpolate 内のunseeded global random state | warning | medium |
| `MLD303` | profile platformと異なるabsolute asset path | warning | high |
| `MLD304` | 複数renderer profile対象でcleanup・fixed・camera semanticsが分岐するのにguardなし | warning | medium |
| `MLD305` | asset pathのcase-only mismatch | warning | high |
| `MLD306` | fontが対象platform/profile allowlistにない | info | high |
| `MLD307` | frame callback内のwall clock、filesystem、network I/O | warning | medium |

`MLD301` は一引数 updater を禁止する rule ではない。absolute dependency は正しい。

```python
dot.add_updater(lambda m: m.next_to(driver))  # absolute; 通常は問題なし
dot.add_updater(lambda m: m.shift(0.1 * RIGHT))  # FPS 依存
dot.add_updater(lambda m, dt: m.shift(dt * RIGHT))  # 時間基準
```

`MLD302` は literal seed、local `random.Random(seed)`、local NumPy Generator のseedを追う。seed済みgeneratorをglobal randomと同じ非決定扱いにしない。`MLD304` は `--profile all` 等で複数rendererを実際に対象とした時だけ有効で、Cairo一件だけを明示したprojectにrenderer guardを要求しない。

## 8. CLI、設定、抑制

> **superseded.** CLI flag、設定 key、JSON/SARIF 契約の正典は
> [`docs/reference/cli.md`](docs/reference/cli.md)、
> [`docs/guides/configuration.md`](docs/guides/configuration.md)、`schemas/`、
> および `qual --help` である。本節の一覧には出荷済み flag の欠落がある
> （`--analysis-summary` など）。

### 8.1 commands

```text
qual check [PATH...]
qual explain RULE
qual rules
qual config
qual cost PATH [--scene NAME]
qual coverage [PATH...] [--format text|json]
qual static-facts [PATH...] [--profile NAME|all] [--renderer cairo|opengl] [--fps FPS] [--resolution WIDTHxHEIGHT]
qual change-impact --before PATH --after PATH [--profile NAME|all] [--renderer cairo|opengl] [--fps FPS] [--resolution WIDTHxHEIGHT]
qual source-bridge PATH --request REQUEST.json [--profile NAME|all] [--renderer cairo|opengl] [--fps FPS] [--resolution WIDTHxHEIGHT]
```

`check` options:

```text
--select / --ignore
--min-confidence
--fail-level error|warning|info
--profile
--renderer cairo|opengl
--fps
--resolution WIDTHxHEIGHT
--format concise|full|json|sarif|github
--fix
--unsafe-fixes
--baseline PATH
--write-baseline PATH
--no-cache
--statistics
```

exit code:

- `0`: `min-confidence` 以上かつ `fail-level` 以上の診断なし
- `1`: その終了閾値を満たす診断あり。表示は select / ignore / confidence 設定に従い、info を表示しても `fail-level=warning` なら exit 0
- `2`: CLI / config / internal error

JSON は schema version を必須にし、SARIF は 2.1.0 を外部依存なしで生成する。

`static-facts`は診断を実行せず、`schemas/static-facts-v0.json`準拠の静的意味projectionをstdoutへ出力する。rule selector、suppression、baseline、confidence/fail levelは意味入力ではないため、このcommandにはそれらのoptionを持たせない。成功は常にexit 0、path/config/IO errorはexit 2とする。

`change-impact`はbefore/after双方をfull解析し、`schemas/change-impact-v0.json`準拠の保守的な影響候補をstdoutへ出力する。cacheとruleは実行せず、削除・rename前のedgeをbase graphから保持する。成功はexit 0、入力/config/IO errorはexit 2とする。

`source-bridge`は`schemas/source-bridge-request-v0.json`準拠requestから限定的なpatch候補を生成し、diskへ書かずに仮想適用・full再解析・rematchingを行い、`schemas/source-bridge-v0.json`準拠結果をstdoutへ出力する。成功はexit 0、request JSON/config/IO errorはexit 2とする。候補がunavailable/rejectedでもcontract上の正常結果なのでexit 0である。

`--no-cache` は analysis cache の read / write と cache directory 作成をすべて無効にする。`--fix`、`--baseline`、`--write-baseline`、`--analysis-summary` は source / index state を後段でも必要とするため、cache-v2 では自動的に full analysis を行う。

### 8.2 `pyproject.toml`

```toml
[tool.qual]
manim-version = "0.20"
target-python = "3.11"
select = ["MLC", "MLR", "MLP", "MLD"]
ignore = []
min-confidence = "high"
fail-level = "warning"
default-profile = "production"
knowledge-profile = "local_0_20_1_4d25c031"
respect-manim-cfg = true
exclude = [".venv/**", "media/**"]
per-file-ignores = { "tests/fixtures/**" = ["MLP", "MLD"] }
source-roots = ["."]
stub-paths = []

[[tool.qual.profile]]
name = "production"
renderer = "cairo"
platform = "linux"
working-directory = "."
pixel-width = 3840
pixel-height = 2160
frame-rate = 60
assets-dir = "."
allowed-fonts = ["Noto Sans", "Noto Sans CJK JP"]
cairo-fork-workers = 4
cairo-static-layers = true
video-encoder = "libx264"
transparent = false
antialias = "default"
opengl-readback = "auto"
```

ここで `production` は利用者が付ける実行profile名、`knowledge-profile` は Manim の意味とcapabilityを定義するversioned profile IDであり、別物である。この例は sibling の最適化forkを対象にする。upstream Manim を対象にする設定では `knowledge-profile = "upstream_0_20"` とし、local-only keyは拒否せず「このknowledge profileでは効果を持たない」とconfig表示で明示する。未知のknowledge profileは exit 2 とする。

優先順位:

```text
CLI > selected profile > pyproject base > manim.cfg > builtin defaults
```

renderer 候補が複数なら renderer-specific diagnostic に `applicable_profiles` を付け、「OpenGL profile のみ」のように表示する。

`--profile` 省略時は `default-profile` 一件を解析する。`--profile all` は定義済み全 profile を解析し、同じ根拠の診断を一件へ統合して `applicable_profiles` を列挙する。存在しない profile、重複 name、default 未定義は exit 2 とする。

### 8.3 inline suppression

```python
self.play(...)  # qual: ignore[MLC108]

# qual: ignore[MLP201]
label = always_redraw(...)

# qual: file-ignore[MLP]
```

- 行末 comment は同じ statement。
- standalone comment は次の statement。
- file-ignore は header 領域、すなわち shebang、encoding declaration、module docstring が終わるまでに置ける。
- config 内の未知 rule ID は exit 2。inline suppression の未知 ID は専用 warning とし、対象診断を抑制しない。
- 後から `--warn-unused-ignores` を追加する。

baseline fingerprint は line number を使わず、`rule ID + relative path + qualified Scene + surrounding token hash` で作る。行追加だけで baseline が全失効しないようにする。

### 8.4 StaticFacts v0 public contract

Poietra / fast-manim 向けの静的意味情報は、内部の `FileId`、`ObjectId`、`PlayGroupId`、heap、cache entry をserializeせず、[`docs/rfcs/0001-static-facts-v0.md`](docs/rfcs/0001-static-facts-v0.md) と [`schemas/static-facts-v0.json`](schemas/static-facts-v0.json) に定義したversioned projectionとして`qual static-facts`から公開する。RFCを正典、JSON Schemaを機械検証可能な形とする。

v0の範囲はScene、reachable object、play/animation、updater、play境界のmembership/render order、renderer risk、coverage frontierに限定する。公開IDはrelative POSIX path、raw source hash、source anchor、bounded call path、cardinality、Scene identityからsnapshot内で決定的に生成し、内部handleを含めない。編集前後の同一性はIDの安定性ではなく後続のrematching契約で扱う。

source anchorはraw bytesのSHA-256、正規化encoding、BOM有無、decoded UTF-8 text上のend-exclusive byte range、1-based line / Unicode scalar columnを持つ。Unknownは`null`にせず、必ず非空の`reasons`配列を持つ。内部の`Num` / `Truth` / `Presence`を全面変更する必要はなく、projection生成時のprovenance sidecarで理由を合成してよい。

provenance sidecarはCFG/call/lifecycle factから実際に確認できた原因だけを記録する。`Maybe`や`Unknown`というlattice値だけから`branch-join`、`loop-widening`、candidate cap等を推測してはならない。branchとloopのように複数原因が実在する場合はdeduplicateしたreason集合を保持し、原因factを保持していないfieldは`unsupported-semantics`へ落とす。

StaticFacts producerはrule selection / suppression / baselineから独立して必要なfact capabilityを全て計算する。初期producerはcacheを経由しないfull analysisとして同じraw byte snapshotをparseとhashへ渡す。semantic dependency graphを接続してincremental producerを追加する時も、同一snapshotについてfull / incremental、cache状態、worker数が異なるJSONはbyte-identicalでなければならない。renderer riskはdynamic call、unknown animation target、active updater、`always_redraw`、dynamic wait / stop condition、camera mutation、external state / I/O、randomness、unknown write channel、unknown render orderを報告するが、`safe_to_skip_render`や`safe_to_fork`などの最適化許可は公開しない。

semantic dependency graphはcacheではなくfact layerとして所有し、[`docs/rfcs/0002-semantic-dependency-graph-v0.md`](docs/rfcs/0002-semantic-dependency-graph-v0.md) を契約とする。辺の正規方向は常にdependentからdependency（callerからcallee、Sceneからentrypoint、対象objectからplay）とし、同じ辺から決定的なforward/reverse indexを構築する。解決不能なdynamic call、base、import、definition attributionは推測した辺にせず、所有node・reason・anchorを持つUnknown frontierとして残す。cache component partitionはfile間edgeを無向に見た弱連結成分だけを利用し、ChangeImpactはbefore/after snapshotのreverse edgeを利用し、外部JSONは内部handleではなくStaticFactsのsnapshot IDとsource anchorへprojectionする。Runtime ID、Static/Runtime最終照合、gesture意味論、TracePlan、checkpoint、visual validationは本repositoryの責務外とする。

### 8.5 ChangeImpact v0 public contract

[`docs/rfcs/0003-change-impact-v0.md`](docs/rfcs/0003-change-impact-v0.md) と [`schemas/change-impact-v0.json`](schemas/change-impact-v0.json) を外部契約とする。入力はbefore/afterの2 source snapshotを必須とし、両方を独立にfull解析する。raw hashでadded/removed/modified fileを、qualified name・definition kind・relative path・definition source sliceでchanged definitionを判定する。renameは推測せずremoved + addedとして表す。

changed file/definitionをbase/target各graphのreverse traversal originとし、base側は削除済みedge、target側は新規edgeを保持する。出力候補は`base | target`を明記したStaticFacts Scene/play/object ID、source anchor、originからのreason pathを持つ。異なるsnapshotのIDを同一視せず、cross-snapshot rematchingはP1へ残す。

到達したdynamic call、unresolved base/import、definition attribution不能、decode/parse failureは非空`reasons`配列を持つUnknown frontierとして投影する。semantic configまたはknowledge profileが異なる場合は両snapshotの全source semanticsをoriginに広げ、`semantic-config-changed` frontierを返す。frontierがなければ`completeness=complete`、一つでもあれば`candidates`とする。これは候補集合の静的coverageであり、編集意図や描画最適化許可ではない。

`self.play(*animations)`はanimation/target列を完全とは扱わず、Playに`star-arguments` frontierを付ける。dependency graphでは同じPlayから所有Sceneの全reachable objectへ`starred-animation-target`候補辺を張り、ChangeImpactを`candidates`へ落とす。

### 8.6 SourceBridge / rematching v0 public contract

[`docs/rfcs/0004-source-bridge-v0.md`](docs/rfcs/0004-source-bridge-v0.md)、[`schemas/source-bridge-request-v0.json`](schemas/source-bridge-request-v0.json)、[`schemas/source-bridge-v0.json`](schemas/source-bridge-v0.json)を外部契約とする。v0 templateはliteral call argument置換、一意bindingの既存`.shift(ARG)`引数置換、allocation call直後の`.shift(ARG)`挿入に限定する。dynamic existing argument、複数binding、不明allocation、hash/snapshot不一致では推測しない。複数source候補はmedium confidenceで全件を返し、自動選択しない。

各editはpath、raw source hash precondition、encoding-aware anchor、`original_text`、replacementを持つ。command自身はdiskへ書かず、`original_text`をapplication guardとrollback payloadの双方に使う。候補ごとにmemory上で適用し、元encodingへ再encode、全sourceを再parse・frontend/lifecycle/StaticFactsまで再解析する。

rematchingは編集前後IDの一致を要求せず、Scene identity、entity kind、edit-adjusted source range、bounded call path、cardinality、kind/binding候補を使う。結果は`match | ambiguous | missing`で、ambiguous時は全candidate IDを返す。parse valid、単一match、coverage preservedを全て満たすcandidateだけacceptedとする。新規Unknown/frontier、coverage低下、parse failure、ambiguous/missingは理由付きrejectedとする。任意Python意味保存、external write、gesture意味論、runtime照合、visual validationは責務外である。

## 9. cache と並列性

MVP はまず逐次で正しさを確立した。10k-LOC cold measurement で lifecycle が支配的と確認した後、cache-v1 は call graph の同じ bottom-up layer にある非再帰 summary、独立な Scene lifecycle、独立な rule を bounded worker pool で並列実行する。再帰 SCC は依存順の逐次 fixpoint のままにし、Scene と rule の結果は宣言順にcollectして最後に必ず安定sortする。worker数が変わってもdiagnostic / JSONはbyte-stableでなければならない。module parse / project index はまだ逐次であり、追加の並列化は測定後に行う。

cache-v2:

```text
.qual-cache/cache-v2.sqlite3
```

第一層はcache-v1と同じatomicなwhole-project entryである。全sourceが一致する通常のwarm runはfrontendを起動せず、次を再利用する:

- selector、suppression、confidence filter 適用後かつ baseline 適用前の diagnostics JSON
- literal asset resolution / SVG inspection が参照した file content hash
- missing path と case scan が参照した directory entry hash

whole-project key:

```text
tool/schema/build version
+ target Python を含む resolved semantic config hash
+ Manim knowledge profile hash
+ sorted relative source paths + source content hashes
```

第二層はwhole-project miss時のincremental component entryである。全sourceをdecode / parseし、module tree、exports、class hierarchy、qualified callsと`SemanticDependencyGraph`のfrontend部分を再構築してから、そのgraphのproject-local import、qualified call、resolved base class、module-name collision edgeを無向に見た弱連結componentを作る。cache module自身はこれらの意味依存を再発見しない。cross-file helperの診断がcallee側spanへanchorされ得るため、primary pathだけを単独のcache shardにはしない。一つでも静的な意味依存edgeがあれば同じcomponentとして無効化する。

component entryに保存するもの:

- component内callableの`MethodSummary` JSON
- componentが所有する、filter後かつbaseline前のdiagnostics JSON
- component内のliteral asset / case scan dependency manifest JSON

AST、token、source text、heap snapshotは保存しない。`FileId`を含むsummaryは、全projectのsorted relative source layout hashをkeyに含め、layoutが変われば全componentをmissにする。component key:

```text
tool/schema/build version
+ resolved semantic config hash
+ Manim knowledge profile hash
+ sorted project source layout hash
+ sorted component source paths + source content hashes
```

componentはproject-local summary dependencyの推移閉包を含むため、このcomponent content hashがimported summary hashを兼ねる。whole-project missではhit componentのsummaryをseedし、miss componentのsummaryと、そのcomponentが定義するScene lifecycle / cost factsだけを再計算する。ruleは決定的な同じproject contextをqueryするが、新規diagnosticとして採用するのはmiss component所有pathだけとし、hit componentの保存済みdiagnostic shardと結合して最後に安定sortする。componentが一部だけhitした実行は内部statusを`partial`とする。

lookup 時は source key に加えてentryごとのdependency manifestを再計算し、asset の作成・削除・内容変更・case-only path の変化でも必ず miss にする。source bytes は key 作成と解析で同じ snapshot を使う。SQLite WAL を使い、同じ project への並行 cold writer を許容する。entry は lookup / store ごとの単調な access sequenceで、recent 16 whole-project snapshotsとrecent 256 component snapshotsに制限し、store後に古いものを削除する。DBまたは保存JSONのparse破損時はstderrに警告してDBを削除・再構築し、component外の`FileId`を含む構造不正entryはそのentryだけを削除・再構築する。その他のcache I/O failureはその実行だけcacheを無効にし、必要なcomponentまたはfull analysisを続ける。cacheは正しさに必要な状態ではなく、いつでも捨てられる派生物とする。

## 10. repository layout（削除済み）

この節は 78 行の Python source tree（`src/qual/*.py`、`tools/`、`tests/unit/`）
だった。実装は Rust であり、記載されたパスは一つも存在しない。誤誘導しかしない
ため本文ごと削除した。実際の配置は
[`docs/architecture.md`](docs/architecture.md) が正典である。

## 11. テスト戦略

> **superseded.** fixture の実際の配置は `tests/fixtures/rules/<ID>/` と
> `tests/rules_*.rs` の golden test であり、本節が書く `branches.py` /
> `expected.json` という名前ではない。§11.4 の release gate（発火候補 200 件の
> 人手 label、precision 98%、95% Wilson 下限 95%）はどのコードも計算していない。
> 実際に強制されているのは `tests/corpus_gate.rs` の corpus 件数下限と、
> `CONTRIBUTING.md` が定める labeling protocol である。正典は
> [`CONTRIBUTING.md`](CONTRIBUTING.md) と `tests/`。

### 11.1 rule fixtures

```text
tests/fixtures/rules/MLC108/
  invalid.py
  valid.py
  branches.py
  expected.json
```

各 rule は最低限、true positive、near-miss、alias import、branch Unknown、suppression を持つ。

### 11.2 必須の test layers

1. 日本語を含む source の UTF-8 byte column / character offset 変換
2. explicit alias、star import、relative import、shadowing、inheritance
3. CFG の branch join、loop widen、try/finally
4. lifecycle state snapshot
5. 各 rule の golden diagnostic
6. config precedence と suppression
7. JSON / SARIF schema validation
   - StaticFacts v0はrepresentative fixtureと実producer出力をDraft 2020-12 validatorで検証し、理由なしUnknownと未知fieldをrejectする
8. fix 後の parse、二回目が no-op になる idempotence
9. knowledge profile と対象 Manim source の drift check
10. 実 corpus の false-positive regression
11. cold / warm linter benchmark

corpus には Manim 公式 example / test に加え、提供者の許可を得た実プロジェクトの匿名化 snapshot を入れる価値がある。特に Text / MathTex、Graph、短い多数 play、updater の性能 rule を現実的に検証できる。ただし公開リポジトリの証跡としては、再配布可能なソースのみを収録する。

### 11.3 rule 仮説の動的検証

linter 本体は render しない。一方、rendering rule の開発 CI では小さな fixture Scene を fresh subprocess で Cairo / OpenGL render し、次を確認する nightly suite を置く。

- 期待する例外 class / message
- frame count / Scene membership trace
- representative frame hash または tolerant image diff
- renderer 間差
- warning が提案する修正版の挙動

OpenGL context は test node ごとに fresh process を原則とする。対象環境では GLX が process ごと落ち、EGL は動くことが実証されているため、headless profile は EGL を明示する。

### 11.4 quality gates

- `tests/corpus/manifest-v1.json` に source digest、license、期待診断、label revision を固定する。Manim公式examples/testsと、許可済み実Scene snapshotを含める。
- default correctness rules全体で、少なくとも200件の発火候補を人手labelし、各ruleに最低10 true-positive例を持たせる。precision点推定98%以上かつ95% Wilson下限95%以上、さらにpinned公式corpusで既知false positive 0件をrelease gateとする。
- performance advisoryは上記と別集計にし、precisionに加えてmultiplicity evidenceと代替案が有用かをreview checklistで採点する。
- `tests/corpus/benchmark_10kloc/` を固定し、`benchmarks/reference-machine.json` のCPU/OS/Pythonでcold 10k LOC 2秒以内、cacheを温めた直後の二回目をwarm 0.5秒以内、20個の独立componentのうち1fileだけを変更したincremental runを0.5秒以内とする。benchmark は隔離した一時projectで、cold前に`.qual-cache`不在、coldがcache miss、warmがcache hit、incrementalがpartial hitであることもassertし、filesystem cache条件を記録する。
- peak RSS: 300 MiB 未満
- diagnostic order と JSON は同じ入力で byte-stable

## 12–13. 実装 roadmap と初期 backlog（削除済み）

この二節は実装前に書かれた phase 0–6 の計画と issue-sized backlog だった。
catalog は 92 rules / 0 reserved で完了しており、計画としての役割を終えたため
本文ごと削除した。現在の作業手順は [`CONTRIBUTING.md`](CONTRIBUTING.md) が正典で、
進行中の作業は GitHub issues が唯一の一覧である。

## 14. 参照すべき Manim source map

行番号は冒頭の workspace snapshot に対するもの。実装時は function / class 名を正とする。

| 意味 | source anchor |
| --- | --- |
| Scene 全体 lifecycle | `manim/scene/scene.py:263` `Scene.render` |
| Scene updater 順序 | `manim/scene/scene.py:419` `update_mobjects`; `:436` `update_self` |
| Wait freeze 判定 | `manim/scene/scene.py:455` `should_update_mobjects` |
| Scene family / add / remove | `manim/scene/scene.py:503`, `:527`, `:583` |
| Scene child removal時のroot分解 | `manim/scene/scene.py:727` `restructure_mobjects`; `:770` `get_restructured_mobject_list` |
| Scene updaterのCairo警告 | `manim/scene/scene.py:681` `add_updater` |
| non-introducer auto-add | `manim/scene/scene.py` `add_mobjects_from_animations` |
| moving/static suffix | `manim/scene/scene.py:935` `get_moving_mobjects`; `:990` `get_moving_and_static_mobjects` |
| play argument conversion | `manim/scene/scene.py:1012` `compile_animations`; `manim/animation/animation.py:2106` `prepare_animation` |
| frame time grid | `manim/scene/scene.py:1108` `get_time_progression` |
| duration validation | `manim/scene/scene.py:1157` `validate_run_time` |
| Wait API | `manim/scene/scene.py:1266`; `manim/animation/animation.py:2147` |
| play preparation / begin | `manim/scene/scene.py:1336`, `:1384` |
| per-frame play loop / cleanup | `manim/scene/scene.py:1460` `play_internal` |
| updater / interpolation order | `manim/scene/scene.py:1840` `update_to_time` |
| Animation begin/finish/cleanup | `manim/animation/animation.py:264`, `:283`, `:296`, `:311` |
| Animation per-frame interpolation | `manim/animation/animation.py:423` |
| Transform copy / align / replacement | `manim/animation/transform.py:1066`, `:1116` |
| Mobject child validation | `manim/mobject/mobject.py:418`, `:761` |
| Mobject copy / updater recursion | `manim/mobject/mobject.py:1178`, `:1203` |
| updater registration / removal | `manim/mobject/mobject.py:1299`, `:1382`, `:1409`; signature check `:286` |
| family traversal | `manim/mobject/mobject.py:2846`, `:2929` |
| `.animate` builder | `manim/mobject/mobject.py:3768` |
| target / saved-state protocol | `manim/mobject/mobject.py:1193`, `:2431`; `manim/animation/transform.py:3111`, `:3255`, `:3281` |
| z-index / front ordering | `manim/mobject/mobject.py:3697`; `manim/scene/scene.py:880` |
| family / point alignment | `manim/mobject/mobject.py:3332`; `manim/mobject/types/vectorized_mobject.py:2006` |
| renderer別 VMobject point layout | `manim/mobject/types/vectorized_mobject.py`; `manim/mobject/opengl/opengl_vectorized_mobject.py` |
| curve insertion / length | `manim/mobject/types/vectorized_mobject.py:1975`, `:2112` |
| Create VMobject constraint | `manim/animation/creation.py:105` |
| AnimationGroup timing | `manim/animation/composition.py:174`, `:197`, `:213` |
| always_redraw reconstruction | `manim/animation/updaters/mobject_update_utils.py:67` |
| updater Animation callbacks | `manim/animation/updaters/update.py` |
| TracedPath growth | `manim/animation/changing.py` `TracedPath` |
| Text construction | `manim/mobject/text/text_mobject.py:480` |
| font registration context | `manim/mobject/text/text_mobject.py:1442` `register_font` |
| MathTex → TeX/SVG | `manim/mobject/text/tex_mobject.py`; `manim/utils/tex_file_writing.py` |
| SVG parse | `manim/mobject/svg/svg_mobject.py:431`, `:466` |
| ParametricFunction sampling | `manim/mobject/graphing/functions.py` `ParametricFunction.generate_points` |
| ImageMobject construction / Cairo capture | `manim/mobject/types/image_mobject.py`; `manim/camera/camera.py` image display path |
| Cairo display selection/raster | `manim/camera/camera.py:720`, `:800`; `manim/renderer/cairo_renderer.py:1691` |
| play hash | `manim/utils/hashing.py:750` |
| 3D camera / fixed objects | `manim/scene/three_d_scene.py:312`, `:330`, `:357`, `:379`, `:399` |
| moving camera renderer contract | `manim/scene/moving_camera_scene.py:104` `MovingCameraScene` |

採用した calibration evidence はこのrepoの `docs/research/perf-evidence.md` にsource digestとともに固定した。詳細な元監査は作成時点では sibling repo の次にあった。

- `../manim/.tmp_perf_audit_execution_log.md`
- `../manim/.tmp_perf_audit_candidates.md`
- `../manim/.tmp_perf_audit_deep_dive.md`

`.tmp_*` は一時ファイルであり、linter実装の必須入力にはしない。設計上は execution log の「採用」だけを真理として hardcode せず、そこで得た stage 分解と乗数を cost taxonomy に使い、machine 固有の秒数は versioned calibration evidence として分離する。

## 15. 実装時に守る不変条件

1. 対象コードも Manim も import / exec しない。
2. Unknown を根拠に certain/high error を作らない。
3. Scene membership と visibility を同じ boolean に潰さない。
4. Animation construction と play lifecycle effect を同じ時点にしない。
5. introducer / remover / replacement と auto-add を knowledge profile に明記する。
6. live Mobject と starting / target copy の identity を分ける。
7. frame callback と play-start operation の frequency を混同しない。
8. renderer-specific diagnostic は適用 profile を必ず出す。
9. performance の未知値から偽の精密 ms を作らない。
10. autofix は parse 検証し、SAFE と UNSAFE を混ぜない。
11. source span は Unicode で round-trip test する。
12. diagnostic order と serialized output を deterministic にする。

この不変条件を満たした小さな rule set を先に出し、その後 lifecycle、cost、renderer の順に深くするのが最短経路である。
