# パネルとRust本体のIPC v1

判断日: 2026-10-06。採用日: 2026-10-08。更新日: 2026-10-10。状態: **accepted**。これは `frameeyeosc` / `frameeyeosc-panel` 間のIPC v1実装仕様。設定・状態交換と手動キャリブレーション契約を実装済み。
対象は設定と状態の交換、手動まぶたキャリブレーションの操作・進捗・結果。新しい出力モード、profile削除、自動学習、パネルUI全体は対象外。

## 採用した方針と判断根拠

Unix pathname socket上のNDJSONを使い、設定・profileを保存するwriterはRust本体だけとする。状態取得、設定変更、測定開始・cancelの要求にID対応の応答を返し、追跡・配信・測定段階・残り時間・結果を通知する。
設定はpatch項目だけ更新し、不正更新では以前の有効設定を維持する。接続復旧時は最新snapshotを取得し、応答未確認の操作を自動再実行しない。
測定は各段階をユーザーが手動で開始し、準備2秒・計測5秒、閉眼→自然開眼→任意の見開きとする。見開きの省略と途中cancelは必須。
eventの因果ID `caused_by` は測定状態・最新結果にも保持し、再接続後に原因を照合できるようにする。応答対応用のidとは区別する。
開始接続の切断時は測定cancel・旧profile維持、測定中のABI変更は拒否とする。ABI更新失敗では旧設定・入力を保持し、再検証により継続または待機する。宛先・prefix切替時の追加停止通知は行わず、パネルで実際の送信状態と切替結果を確認する。配信切断・停止でも常駐する起動終了方針は[パネルとOSCの起動終了](../research/2026-10-03-frameeyeosc-features.md#パネルとoscの起動終了)に記録する。

数値変換と有効サンプル率80%以上は[まぶた設計](2026-10-03-eyelid-calibration-design.md)、設定一覧と反映動作は[機能調査報告](../research/2026-10-03-frameeyeosc-features.md)に従う。追加余裕左右0.10・最小基礎幅0.10・最小見開き幅0.05は変更可能な初期値。新しい測定段階や出力モードは追加しない。

## 接続とメッセージ

本体はpathname socketをlistenし、パネルは同じパスへ接続する。既定パスは `$XDG_RUNTIME_DIR/frameeyeosc/control.sock`、専用ディレクトリは0700・socketは0600とする。XDG_RUNTIME_DIRがなければ明示パスを必要とし、未指定なら起動エラー。同じsocketパス用の排他lockを取得してから、残存socketに生存instanceがないことを確認してstaleなファイルだけをunlink・bindする。lock取得失敗や生存socketでは新instanceを起動せず、生存socketを勝手に削除しない。
UTF-8のJSON objectを1行1件・LF終端で送る。読み取りは行単位に組み立て、分割readや複数行の同時readを許容する。JSON内の改行はescapeする。非有限数・重複キー・壊れたJSONは受理しない。
全メッセージに `v:1` と `type` を持つ。requestは `id`（接続内で一意の空でない文字列）、`method`、`params` object。responseは同じ `id`、boolの `ok`、常にobjectの `result`、`error` を持つ。成功時はok=true/error=null、検証・準備・保存等の失敗はok=false/error=`{code,message}`を必須とし、返す状態がない未知要求等でもresult={}とする。
eventは `event`、`data` object、必須の `caused_by` を持ち、応答対応用のidは持たない。caused_byはnullまたは `{connection_id,request_id}`。本体が受け付けた原因操作の接続とrequest.idから生成し、クライアントに任意の因果IDを指定させない。未知versionは本体自身のv=1で `unsupported_version`、未知methodは `unknown_method` を返し実行しない。未知params/patchキーは拒否する。パネルは未知eventおよび既知eventの未知補助フィールドを無視する。
JSONを解析できない要求は接続を終了し、部分的な設定を適用しない。解析可能な不正要求は識別できるidを返し、id自体が不正ならresponseのidはnullとして `invalid_request` を返す。

```json
{"v":1,"type":"request","id":"r1","method":"state.get","params":{}}
{"v":1,"type":"request","id":"r2","method":"settings.patch","params":{"patch":{"eye_widening":false,"eyelid_output_cap":0.75},"save":true}}
```

本体は全接続の変更要求・測定timer・状態反映を1つのglobal直列順序で処理し、再接続時に新旧接続が一時的に重なる場合も同じ順序に従う。要求IDと応答cacheは接続ごと。同じid・同じ解析済みmethod+params（objectキー順序は無関係）を再受信したら保存した応答を返し、測定開始・保存・再接続を再実行しない。同じidで異なる内容は `id_conflict`。未完了要求の重複も同じ処理結果へまとめる。
cacheは接続内の全受付済みidを保持し、evictしたidを再実行する方式は使わない。新idを接続あたり4096件まで受け付け、上限後の新idは実行せず `request_limit` を返す。既存idの応答は引き続き再送可能。4096はIPC v1の接続内受付上限であり、パネルは再接続してstate.getから照合する。cacheは切断時に破棄し、再接続をまたぐexactly-onceは保証しない。

## 状態と設定

`state.get` のresultは最新snapshotそのものとする。接続後の最初の要求はこれとし、最初のstate.get応答を送るまではその接続にeventを送らない。応答のsnapshotと同じglobal直列化点で暗黙の通知購読を開始し、応答の後から変更通知を送る。別のrevision・履歴再送プロトコルは追加しない。

| snapshotフィールド | 型・意味 |
| --- | --- |
| `instance_id` | 本体の起動ごとに変わる文字列。再起動前の測定IDと区別 |
| `connection_id` | 本体が割り当てる受信接続の文字列ID。instance内で一意、再接続時は新ID |
| `settings.current` | 全項目の現在の適用値 |
| `settings.saved` | 保存済みの明示値。未保存項目は省略 |
| `settings.sources` | 各現在値の更新元 `cli` / `saved` / `default` / `panel` |
| `tracking` | `active` / `inactive` / `unknown`。既存のglobal追跡状態 |
| `stream` | `{state,peer}`。stateは `connected` / `disconnected` / `unknown`、peerは観測した配信peerのIP・port文字列またはnull |
| `osc_target` | 実際のOSC送信先のIP・port文字列またはnull。配信peerのportをOSCへ流用しない |
| `output` | `{state,reason}`。本体の送信状態 `sending` / `waiting` / `inactive` と、後述の理由 |
| `active_profile` | 未校正ならnull、そうでなければ現在適用中の左右profileと派生値。測定中の候補とは区別 |
| `calibration` | 未開始ならnull、そうでなければ後述の状態object |

streamは配信peerの観測状態であり、ゲームの生存やOSC受信成功を保証しない。分類できない一時切断と意図した終了をこのIPCで推定して決めない。
outputは次の順で判定する。入力を開けない・未準備ならwaiting/reason=`input_unavailable`、現在の共有メモリversion/sizeが選択ABIに一致しなければwaiting/reason=`abi_mismatch`、実際の送信先が未確定ならwaiting/reason=`no_target`とする。以上の条件を通過しても片眼raw欠落・非有限などsnapshot全体が無効ならinactive/reason=`invalid_sample`、それ以外でglobal trackingがinactive/unknownならinactive/reason=`tracking_inactive` / `tracking_unknown`とする。入力の検証、送信先の確定、両眼を含むglobal追跡・snapshotの有効性がすべて揃う場合だけsending/reason=nullとする。複数条件が同時に成立するときのreasonもこの順序で選ぶ。sendingは本体がUDP送信を試行する状態であり、受信側の受信成功を意味しない。inactive/waiting時の既存停止通知もoutputをsendingには変えない。
active_profileは `left` / `right` objectを持ち、各眼に有限な `closed`、`relaxed`、`relaxed_max`、`wide`（未測定・無効測定ならnull）、現在のmarginから求めた `d` / `wide_start`、boolの `wide_available`、`wide_unavailable_reason`（有効ならnull、無効なら `not_measured` / `invalid_wide` / `insufficient_interval`）を含める。これによりパネルは実適用の見開き開始値と利用不可理由を表示できる。区間不足でも品質条件を満たした有限な測定wide値は保存・保持し、margin変更ごとにd/wide_start/wide_availableと理由を再計算する。利用不可になった測定値は捨てず、marginを再調整すれば再判定できる。raw全サンプルは送らない。state.changedも同じsnapshotを返す。
設定キーのv1は `eye_widening`、`eyelid_output_cap`、`eye_widening_margin_left/right`、`min_base_width`、`min_wide_width`、`prefix`、`one_euro`、`one_euro_min_cutoff`、`one_euro_beta`、`one_euro_d_cutoff`、`target_mode`、`target_host`、`target_port`、`shm_version`。列挙外のpatchキーは引き続き拒否する。
boolと数値・文字列を厳密に区別する。capは有限0..1、marginは有限非負、min_base_width/min_wide_widthは有限かつ0より大きく1以下、One Euroのcutoffは有限正・betaは有限非負、prefixは既存OSCパス条件、portは整数1..65535。target_modeは `auto` / `manual`、shm_versionは文字列 `auto` / `4` / `5` とする。manualでは有効なhost/IPを必要とし、関連する項目はpatchを現在値へmergeした候補全体で検証する。

`settings.patch` のparamsは `{patch,save}`。saveはboolで明示し、trueならpatchに含まれる項目だけを保存する。現在値の全体を保存してCLI初期値を他項目へ広げない。保存設定の既知・未知の未変更項目は保持する。
検証と必要な入力/UDPの準備を済ませ、save=trueなら設定ファイルをatomic replacementしてから候補を現在値へ反映する。検証・準備・保存失敗では変更全体を適用せず、旧現在値・保存値を維持する。save=falseなら現在値だけ更新する。
応答のresultは `current`（全適用値、実際のprefixを含む）、`sources`、`osc_target`、`output`、`save`（`saved` / `not_requested` / `failed`）を返す。適用前に拒否した場合のsaveは `not_requested`、保存を試みて失敗した場合は `failed`。settings.patchの失敗時もok=false/errorと旧設定を保持した実際のcurrent/sources/osc_target/output/saveを返し、パネルは送ったpatchを適用済みと推測しない。

起動時は明示CLI > 保存設定 > 既定値。稼働中のパネル明示更新はCLI由来の現在値も上書きできる。次回同じCLIを指定すれば起動時に再優先される。CLI未指定と明示falseを区別し、保存値・現在値・更新元を別に表示する。
見開き合成・cap・margin・最小幅・prefixは即時反映する。marginは左右別に保存・指定し、d/wide_startは派生値。margin・最小幅変更時とOne Euroの有効/無効・3係数変更時は対応フィルタ履歴をresetする。現在profileの基礎区間、またはactive sessionの成功済みclosed/relaxed候補の基礎区間を無効にする最小基礎幅の更新はpatch全体を拒否し、旧設定・profile・成功候補を保持する。relaxed成功前には完成した候補基礎区間がないため、後のrelaxed段階を現在設定で検証する。最小見開き幅による区間不足は更新を受理し、その眼のEyeWide=0とする。
targetのauto/manual・host/port変更は宛先再評価とUDP再接続、ABI変更は入力再openとversion/size検証を行う。autoでpeer未検出ならosc_target=nullの待機状態を返し、未検出だけを設定の不正とは扱わない。stream.peerとosc_targetは別々に更新し、manualのOSC送信先を配信peerと混同しない。
宛先またはprefixを変更するとき、切替前の宛先・prefixへ `EyeTrackingActive=false` を送る追加の停止通知は行わない。パネルはsettings.patch応答とstate.changedのsettings.current.prefix・osc_target・outputで実際の切替と送信状態を確認する。この判断の対象は切替時の追加通知だけで、通常のtracking inactive時に既存コードが行う停止通知は削除しない。
ABIの候補入力を開く・検証する処理が失敗した場合は旧設定・保存値・入力を保持して変更を拒否する。ただしOSCを継続できるのは、旧ABIで現在の共有メモリversion/sizeの検証が通り、宛先とglobal追跡の有効条件も満たす場合だけ。不一致ならglobal inactiveで送信を待ち、outputはwaiting/reason=abi_mismatchとする。候補ABIを保存値だけへ反映しない。

## キャリブレーション要求と状態遷移

本体全体でactiveな測定sessionは1件だけとする。新session開始要求は既存activeがあれば `busy`。IDはinstance内で一意にし、各操作は `session_id` を明示する。

| method | params | 成功時の遷移 |
| --- | --- | --- |
| `calibration.start` | `{}` | 新sessionを作り `waiting` / stage=`closed`。採取はまだ開始しない |
| `calibration.begin_stage` | `{session_id,stage}` | 待機中の同じstageを `preparing` → 2秒後 `measuring` → 5秒後検証 |
| `calibration.skip_wide` | `{session_id}` | `waiting` / stage=`wide` でのみ候補profileを保存・適用 |
| `calibration.cancel` | `{session_id}` | activeな候補を破棄し `cancelled`。旧profileを保持 |

begin_stageのstageは `closed` / `relaxed` / `wide`。closed成功後はrelaxed待機、relaxed成功後はwide待機とし、自動で次段階を開始しない。wide待機ではユーザーが開始か省略を選ぶ。
準備中・測定中のbegin_stageや順序の異なるstageは `invalid_state`。不明または保持していない旧sessionは `unknown_session`。completed/cancelled/failedへのcancelはok=false/error=`already_terminal` と実際のcalibration状態を返し、profileを戻した成功cancelとは扱わない。
skip_wideはwaiting/stage=wideだけで受け付け、他のactive状態では `invalid_state`。終端sessionなら `already_terminal` と実際の状態、不明・保持していないsessionなら `unknown_session` を返し、保存を実行しない。
無効サンプルや短いglobal inactiveは欠測として有効数に含めず、1回の無効で測定を即失敗にしない。5秒終了時に有効率不足または基礎区間不正なら `stage_failed` を通知して同じstageのwaitingへ戻す。tracking_lostは診断として記録する。それ以前の成功stageの候補値は保持し、ユーザーが同じstageを再度beginできる。失敗したstageのサンプルは次の採取へ混ぜない。
有効率は5秒の期待producer更新数に対する有効な一意サンプル数の80%以上。poll・同一timestampを水増しせず、固定360標本は要求しない。期待更新数の算定・低頻度時の扱いはproducer更新頻度に基づく実装詳細で、[README](../../README.md#manual-eyelid-calibration)に記録する。このIPC仕様では算定方式を定義しない。

closedとrelaxedが成功しても適用・保存しない。wide完了またはskip_wideを最終操作として基礎区間を再検証し、profileをatomic保存した後で現在profileを置換・まぶた履歴resetし `completed` にする。途中は旧有効profileでOSCを続ける。
profileには測定値だけを保存する。設定の現在値を測定完了時に暗黙に保存せず、設定保存はsettings.patchのsave=trueで明示した項目だけとする。profileファイルは設定ファイル名に `.profile.json` を付けた別ファイルとし、`--config`で保存先を変えた場合も同じ対応を使う。起動時に不正なprofileは診断を出して未校正raw経路で起動し、再キャリブレーションで上書きできる。
wide省略時は新しい基礎区間だけを使い、旧profileのwide値を流用しない。wide測定のサンプル品質が成立しても区間不足（飽和により区間が不足する場合を含む）ならその眼の見開きを無効にし、基礎profileと測定wide値は保存できる。wide=1.0だけを理由には拒否しない。最終適用と後続のmargin・最小見開き幅変更では、現在設定から各眼の利用可否を再判定する。他眼の区間を借用せず、marginを黙って縮めない。
保存失敗はsessionの終端 `failed` / reason=`save_failed` とし、候補を破棄して旧有効profileを維持する。新profile適用・保存の成功を部分的に通知しない。
cancelは待機・準備・測定のどこでも受け付ける。本体が採取停止・候補破棄を完了してから成功応答を返す。最終保存操作とcancelは本体のglobal直列順序で決め、保存完了後に処理されたcancelは `already_terminal` と実際のcompleted状態を返す。成功cancel後に旧sessionの進捗やprofile書換えを起こさない。
sessionを開始した接続が切れたらactive sessionをcancelし、旧profileを保持する。既に完了した保存は取り消さない。IPC切断をパネルの明示Closeと同一視しない。明示Close・ローカルSteamVR終了でパネルとOSC本体を終了する方針は[パネルとOSCの起動終了](../research/2026-10-03-frameeyeosc-features.md#パネルとoscの起動終了)に従う。OSC本体はパネル所有の子プロセスとし、パネル異常終了時も所有本体を終了する。本体異常終了時はパネルを残して停止理由を表示し、自動再起動せずClose後に手動起動する。親終了検知と終了待ち時間は[パネルREADME](https://github.com/mi-24v/frameeyeosc-panel#launch-and-lifecycle)に記録する。
active session中は測定入力を変えるABIのsettings.patchを `busy` として拒否する。他項目と混在したpatchも全体を拒否する。margin変更は測定rawを変えないため許容し、現在profileの派生値を更新して最終保存時も現在marginで判定する。最小基礎幅の変更は上記の現在profile・成功済み候補の両方で検証し、候補が後から区間不正になってwide retryから進めなくなる更新を受理しない。

calibration状態objectは `session_id`、`state`（`waiting` / `preparing` / `measuring` / `completed` / `cancelled` / `failed`）、`stage`、`remaining_ms`、`caused_by`、`result` を持つ。待機・終端ではremaining_ms=0、終端stageはnull。resultは未結果ならnull、stage/終端結果なら `{kind,stage,reason,wide_available,caused_by}` とし、wide_availableは `[左bool,右bool]` の2要素arrayまたは評価前null。result.stageは結果対象のstage（session全体の結果ならnull）であり、次の待機stageへ移った状態objectのstageと区別する。状態と最新resultのcaused_byはそれぞれの原因を保持し、state.getにも返す。snapshot取得要求を過去結果の原因へ置き換えない。
kindは `stage_completed` / `stage_failed` / `completed` / `cancelled` / `failed`。reasonは正常ならnull、失敗時は `insufficient_samples` / `invalid_interval` / `save_failed` 等。`connection_lost` はsessionを開始した接続の切断によるcancelに使う。resultは最新1件をsnapshotにも残し、過去sessionの履歴保存は追加しない。
各calibration操作のresponse.resultは実際のcalibration状態とsave結果を返す。begin/startの成功は受付であり測定成功ではない。skip_wideは保存・適用処理後に応答し、成功ならok=true/state=completed/save=saved、保存失敗ならok=false/errorとstate=failed/save=failedを返す。非同期wide測定後の成功・再試行待ち・保存失敗はeventとsnapshotで確認する。

```json
{"v":1,"type":"request","id":"r3","method":"calibration.start","params":{}}
{"v":1,"type":"response","id":"r3","ok":true,"result":{"calibration":{"session_id":"c1","state":"waiting","stage":"closed","remaining_ms":0,"caused_by":{"connection_id":"p1","request_id":"r3"},"result":null},"save":"not_requested"},"error":null}
{"v":1,"type":"request","id":"r4","method":"calibration.begin_stage","params":{"session_id":"c1","stage":"closed"}}
{"v":1,"type":"request","id":"r5","method":"calibration.cancel","params":{"session_id":"c1"}}
```

## 通知、順序、再接続

最初のstate.get応答後、その接続に `state.changed`（dataは全snapshot）と `calibration.changed`（dataはinstance_idとcalibration状態）を通知する。設定・追跡・配信・本体送信状態の変化はstate.changed、測定中の準備/採取残り時間と結果はcalibration.changedで表す。通知頻度の具体値は実装で決め、毎rawサンプルをIPCへ流す機能は追加しない。
caused_by.connection_idは原因操作の接続で、eventの受信先ではない。data.instance_idで名前空間を区別し、複数接続へ同じ変化を通知するときも原因の組を保持する。state.changedのsnapshot内のconnection_idは各受信接続のIDとし、同じsnapshotをそのまま一括送信して原因接続と混同しない。
calibration.start直後のwaitingはstartを原因とし、段階成功/失敗後のwaitingは対応するbegin_stageを原因とする。準備・採取・進捗・段階結果・wide完了も対応するbegin_stageを原因にする。timerによる派生通知も同じ原因を引き継ぎ、再測定では新しいbegin_stage要求へ更新する。skip_wide・cancel・settings.patchによる変化は各要求を原因にする。無関係なtracking/stream変化はnull。接続切断による自発cancelもnullとし、session_idは保持する。タイミングが近いだけの要求へ原因を付け替えない。
非同期wide完了によるprofile適用と自発的な追跡・配信等の変化が同じ更新で発生した場合は、同じ実際の全snapshotを使い、begin_stageを原因とするstate.changed、nullを原因とするstate.changedの順で通知する。過去の追跡状態と新profileを混ぜた架空のsnapshotは作らない。profile適用だけなら原因付きの1件、自発変化だけならnullの1件とする。この通知順序で、因果保持と実snapshotの契約を両立させる。

以下の通知例は要求例とは独立しており、r5のcancelが成功した後の時系列ではない。最後の切断時cancel通知も他の通知とは独立した例。

```json
{"v":1,"type":"event","event":"calibration.changed","caused_by":{"connection_id":"p1","request_id":"r4"},"data":{"instance_id":"i1","calibration":{"session_id":"c1","state":"preparing","stage":"closed","remaining_ms":2000,"caused_by":{"connection_id":"p1","request_id":"r4"},"result":null}}}
{"v":1,"type":"event","event":"calibration.changed","caused_by":{"connection_id":"p1","request_id":"r4"},"data":{"instance_id":"i1","calibration":{"session_id":"c1","state":"waiting","stage":"closed","remaining_ms":0,"caused_by":{"connection_id":"p1","request_id":"r4"},"result":{"kind":"stage_failed","stage":"closed","reason":"insufficient_samples","wide_available":null,"caused_by":{"connection_id":"p1","request_id":"r4"}}}}}
{"v":1,"type":"event","event":"calibration.changed","caused_by":null,"data":{"instance_id":"i1","calibration":{"session_id":"c1","state":"cancelled","stage":null,"remaining_ms":0,"caused_by":null,"result":{"kind":"cancelled","stage":null,"reason":"connection_lost","wide_available":null,"caused_by":null}}}}
```

最後の例は接続切断による自発cancelの通知で、切断した接続には送れないため、別の購読接続への通知または再接続後のsnapshotで結果を確認する。
本体の状態更新・response・eventの送信順序を同じ直列処理に合わせる。操作による状態通知はそのresponse以降に送り、cancel終端以降は同sessionの準備/測定通知を送らない。cancel前に送信済みの進捗はその応答より前に読む順序を保つ。
パネルは現在の接続・instance_id・session_idの状態だけに通知を反映する。新しい測定開始後に古いsessionの結果を今回の成功として表示しない。
受信接続が変わっても、原因が旧connection_idの保持済み結果は同じinstance_id/session_idのsnapshotとして照合できる。受信接続とcaused_by.connection_idが異なることだけを理由に結果を捨てない。
切断・timeout時は確認前の操作を成功扱いせず、操作を自動再送しない。再接続でstate.getを先に取得し、現在の適用設定・保存設定・測定結果と照合する。本体再起動でinstance_idが変わった場合は古い測定を継続表示しない。

## 検証と適用範囲

- 行の分割/結合、JSON不正・未知version/method/patch、bool/有限数の検証を確認し、不正入力で部分適用しない。
- patch項目だけの保存、CLI初期値の上書き・次回CLI再優先、保存失敗時の旧設定保持、実際のcurrent/sources/osc_target/output/save結果を確認する。
- 一接続の重複idを再実行せず、別内容のid再利用と受付上限超過を拒否する。複数接続・timerのglobal順序、最初のsnapshot応答以前の通知抑止、切断後の照合と未確認操作を再実行しないことを確認する。
- eventの必須caused_by、複数接続での原因保持と受信connection_idの区別、timer・retryの原因引継ぎ/更新、自発通知のnull、再接続snapshot内の測定状態・最新結果の原因保持を確認する。
- 手動closed→relaxed→wide/skip、準備2秒・計測5秒・欠測を含む80%境界、stage失敗後の同段階retryと成功候補保持を確認する。margin増減で測定wideを失わず利用可否を再判定する。
- relaxed成功後の最小基礎幅更新が候補だけを無効にする場合もpatch全体を拒否し、設定・保存値・旧profile・候補を保持する。拒否後にwide完了またはskipで正常保存できることを確認する。
- 途中cancel、保存操作との競合、成功cancel後の進捗停止、wide省略・片眼区間不足、保存失敗時の旧profile保持を確認する。
- busy、開始接続の切断によるcancelと旧profile維持、測定中のABI変更を含むpatch全体の拒否、切替時の旧宛先への追加停止通知を行わないこと、ABI変更失敗時の旧設定・保存値・入力保持と再検証による継続/待機を対応fixtureで検証する。通常のtracking inactive時の既存停止通知も維持する。
- outputの判定優先順位と理由、settings.patch成功/失敗の実際のprefix・osc_target・output、state.changedによる送信状態の変化を確認する。sendingを受信成功として表示しない。

製品IPCのローカル自動検証と、実機パネルのビルド・起動・校正UI表示確認の範囲は[校正検証報告](../research/2026-10-10-calibration-verification.md)に記録する。実機での測定完了・保存・再測定・見開き無効出力、IPC通知の到達・再接続・保存後再起動、製品パネルの常駐復帰・終了・自動起動の確認は残る。複数writer、別プロセス権限、protocol互換性、測定手順が変わる場合に本仕様の接続・保存・状態遷移を再評価する。調査probeの観測は製品IPCの合格を代替しない。
