# 測定準備・数値非依存 runtime / IPC / panel の実装検証

2026-10-09の準備段階の検証記録。採取・評価ツール、ABI入力・送信先検出、設定IPC、独立パネルを確認した。この時点では数値校正・見開き合成は未実装だった。後続の実装・現在の検証範囲は[校正検証報告](2026-10-10-calibration-verification.md)を参照する。

パネルの当時の検証対象は `a2703a850076d5b8a1826042eb65f918c5edb058`（CMake version 0.1.0）。本書は準備段階の結果と技術判断を保持する。

## 要件と検証証拠

| 準備段階の要件 | 成果物 | 自動検証・確認できた結果 |
|---|---|---|
| 後日ローカルで条件別に測定し、候補を明示比較する | [eye_measure.py](../../scripts/eye_measure.py)、[案内](../../README.md#raw-eye-measurement-and-candidate-evaluation) | [Python tests](../../scripts/test_eye_measure.py) 49件。5秒・60秒、session/label、JSONL、必須4候補数値、保存しない評価を提供。fixture replay は実時間品質を証明しない。 |
| producer 停止・重複・逆行・再起動で有効率を水増ししない | Measurement の coverage/counters、timestamp grain 診断 | 期待数は要求 duration / 観測 period。80%境界と1件不足を3原点で検証。欠落・停止再開・粗い時計・中断・mixed/duplicate/overlap/legacy入力を区別。owner-dead partial と同sequenceを捨て次更新のみ採用。 |
| ABI待機・復帰、短い無更新で処理を止めない | [input.rs](../../src/input.rs)、[runtime.rs](../../src/runtime.rs) | v4/v5 packed mmap、未初期化・不一致・delete/resize/recreate・busy・ownerdeath・復帰を検証。10ms scheduling と1秒 inactivity を分離。ABI/保存の3遷移後も通常inactive停止を実UDPで1回通知。 |
| 実配信 peer と OSC 宛先を区別し raw 互換を保持 | [target.rs](../../src/target.rs)、runtime・既存filter | /proc fixtureでowned socket/peer・曖昧候補・再開を検証。manual保持、autoはpeer IPと設定OSC portを組合せる。有限rawの既存clamp、filter後cap、設定切替resetを実UDP確認。 |
| CLI > saved > defaults、保存失敗で部分適用しない | [settings.rs](../../src/settings.rs)、[main.rs](../../src/main.rs) | 明示falseと保存manualのpanel起動、known値検証、atomic save失敗で旧bytes/current/saved保持、未知disk項目保持、整数port・浮動設定snapshotの一致を確認。 |
| Unix IPCの再送・多接続・設定通知が一貫する | [ipc.rs](../../src/ipc.rs)、[IPC tests](../../tests/runtime_ipc/mod.rs)、[現行fixture](../../tests/fixtures/ipc-v1.jsonl)（当時のsubset版から完全版へ置換済み） | 実socketで分割/複数行・duplicate/UTF8/有限JSON・envelope・ID再送/conflict/4096上限・cache先行reserve・2接続順序・cause/receiver ID・half-close・permission・queueを確認。state/settings subset、校正操作は拒否。 |
| 独立パネルでsupported設定を試行・選択保存できる | [panel repo](https://github.com/mi-24v/frameeyeosc-panel)、src/overlay・ipc_client | core / SDK fake runtime / 実Unix testsで10設定、current/saved/sources、draft/Discard/Save/Apply、error/reconnect、非楽観表示を確認。全buttonと10設定行の余白clickは無操作。 |
| hide/配信停止で常駐し、所有childだけを終了する | panel src/child・main、tests/process_test.py・driver.cpp | Close/localQuit/親終了/exec失敗/異常終了/kill escalation・reap、起動競合/foreign peerへ0request・0signal、初回/再接続owned PID、no auto restartを確認。 |
| 独立buildとmanifestを用意する | panel CMake、manifest script・tests、README | x86_64 CTest5件（core、manifest、configure-contract、overlay-runtime、unix-process）PASS。実Rust owned-child/settings/save/約80KiB snapshotと別cwd install/rpath/license smokeはこの準備段階で実施。AArch64はSDK選択configureのみ。 |

当時の検証結果はRust96件（1.33秒）、Python49件（0.395秒）、`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`が終了コード0。パネルのCTestは5/5成功（16.00秒）。Rust/Pythonのhelp、py_compile、diffcheckも確認した。現行の件数・対応機能は後続の校正検証報告に記録する。

## 判断記録

以下は設計上の判断と残る限界。校正追加前の判断は当時の範囲に限定し、現在の契約を上書きしない。

| 段階 | 判断・理由 | 判断が誤っていた場合の影響・残る限界 |
|---|---|---|
| 開始 | IPC state/settings subsetを実装し、未決数値のcalibration/widening/profileは成功表示しない | 完全IPC v1を要求するclientには未対応。架空成功ならprofile/出力を誤認するため strict拒否・nullを保持。 |
| 開始 | ABI既定auto、panel-modeはdefaultsだけautoへ。保存manual/明示CLIを尊重 | 明示auto上書きなら保存宛先が変わり誤送信。autoはheader判定で実producer互換を保証しない。 |
| 開始 | local hardware/SSH/HMD起動は測定準備goalの範囲外、fixturesで検証 | 実測に基づく品質・HMD表示・到達成功を判断できない。実機測定は後続の明示作業。 |
| 測定 | cadenceは観測timestamp由来。external mmap truncateは残存platform hazard | skipped/slow/mixed publishingでnominal rate推定が偏る。チェック間truncateのSIGBUSを防ぎ切れない。 |
| 測定 | v5共通control offsetはfixed fork reference `af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d` のoffset assertionsで裏付け、コードコピーしない | ABIが実producerと異なれば入力不能/誤読。固定source-level証拠はABI5実機成功ではない。 |
| 入力・宛先 | 10msのmutex/futex budgetはscheduling、1秒無updateだけinactivity | 約90Hz正常更新を10msでinactive扱いすると誤停止・filter reset。OS syscall/schedulingの絶対10ms完了は保証しない。 |
| 測定 | overlap/legacy入力は明示unqualifiedで観測を保持。file inode/session+label+conditionで重複除外、replay durationはnull | 独立validationと誤認すれば漏れ率を過小評価。summary-only/未確認qualityは合格を証明しない。 |
| 測定 | 80%を緩和せずepoch/clock uncertaintyを診断。長gapはcoverage損失、短stepはgrainに残す | 境界誤合格/誤不合格やgap由来unknownが起こる。真のhardware時計粒度は観測だけでは証明不能。 |
| 入力・宛先 | metadata statをread前/futex復帰後も保持。1秒限定だとtruncate後のmmap readが先行するため | syscallコストは増える。安全確認を削るとSIGBUS危険増大。保持してもtruncate-betweenchecks hazardは残る。 |
| 入力・宛先 | measured qualityは有限かつ[0,1]をstrict判定、製品rawは有限値を既存clampへ | 混同すると校正qualityに異常値が混入、または既存raw出力互換を壊す。両方の回帰を別に確認。 |
| 入力・宛先 | prepareはABI変更時だけ候補open、旧source保持。manual既resolved宛先は再resolveしない | 無関係patchまでproducer再起動で拒否する、DNS churnでmanual宛先変更する問題を避ける。未解決host/OSDNS処理の絶対期限なし。 |
| 入力・宛先 | stream未検出/曖昧はunknown、detectedだけconnected。peer portとOSC portは別 | 観測不能をdisconnectや送信成功と誤認しない。実vrlinkの/proc観測・実配信到達は未検証。 |
| 設定・IPC | global直列loop、8clients、1MiB queue、4096 IDs・16MiB cache payload、mutation前reserve | 未制限なら遅いpeer/大量JSONで常駐を圧迫。16MiBはrequest/response/ID byte payloadでRSSではなく、allocator/entry overheadは件数で別途有限。 |
| 設定・IPC | 保存は同directoryの0600 temp→fsync→rename、renameをcommit boundaryにする | rename後の失敗をrollbackと報告しない。power-loss時のdirectory-entry durabilityは保証しない。 |
| 設定・IPC | 浮動4設定はf64保持、filter/OSC境界だけf32。portは整数、未知disk/paramsは型を変えない | f32早期変換でcurrent≠savedや範囲外丸め受理。一般の数値を変換するとID cache identity/未知u64保持を壊す。 |
| 設定・IPC | fixtureはsemantic JSON等価、key順序/空白は契約外 | byte一致を要求すると正当serializerを拒否。parsed値の比較とsigned floating zero正規化は維持。 |
| 設定・IPC | IPCにはknown savedだけ、unknownはdisk保持。config read/saveとも64KiB | 未知設定暴露/巨大snapshotや片側だけの上限になる。未知将来キーはround-trip保持する。 |
| 設定・IPC | half-closeは完全行応答をflush後close、transient accept/setup errorsはログ制限して継続 | 正当応答欠落やfd一時不足でOSC終了を防ぐ。idle timeoutは追加せず既存bounded queue、fatal listener errorsは終了。 |
| パネル | request64KiB / response1MiBを方向別にする。snapshot current/saved重複で64KiB超を許す | 同じ64KiBを受信にも適用すると正当約80KiB snapshotを拒否。1MiB上限で無制限受信を避ける。 |
| パネル | request_limit/reconnectはstate.getだけ、不確定patchを自動再送しない。draftは手動Apply | mutation二重実行を避ける。ユーザーが結果を再確認して再操作する必要がある。 |
| パネル | SDKをtarget architectureで選択、AArch64はconfigure-only確認 | host configure成功をARM compile/run成功と誤認できない。NO_SONAME/ORIGINは別cwd実loader smokeで確認。 |
| パネル | owned SO_PEERCREDは同PID。direct backend/exec-replacing wrapperだけ、same socketだけ重複拒否、legacy無管理本体を引継がない | forkするwrapperなら所有PIDとpeer不一致で拒否。旧IPC-less/different socketは事前手動停止しないと二重送信。foreign PIDへsignalしない。 |
| パネル | manifestは絶対ASCIIかつshlex.quoteで変化しないpathのみ、引用・空白・特殊文字を拒否 | 未検証SteamVR argument parserのquote仮定で誤起動する危険を避ける。direct CLI argvのpath制約とは別。実登録/autostart未検証。 |
| パネル | keyboard closeのtoken保証を仮定せずowned opened/handleで処理。image失敗retryは可視時bounded、成功でreset | 誤token仮定で取消不能、無制限retryで負荷増大。実keyboard/画像サイズ受理はHMD未検証。 |

## 後日実行する測定コマンド

実機ローカルで実行する例であり、この準備作業中には実行していない。各条件は2秒準備＋5秒採取。自然開眼は通常瞬きを含む60秒反復とし、再装着後はsession、反復ごとにlabelを変える。

```sh
python3 scripts/eye_measure.py capture --condition closed --session fit-1 --label closed-1 --duration 5 > closed.jsonl
python3 scripts/eye_measure.py capture --condition relaxed --session fit-1 --label relaxed-1 --duration 5 > relaxed.jsonl
python3 scripts/eye_measure.py capture --condition wide --session fit-1 --label wide-1 --duration 5 > wide.jsonl
python3 scripts/eye_measure.py capture --condition relaxed --session fit-1 --label natural-repeat-1 --duration 60 > natural-1.jsonl
python3 scripts/eye_measure.py capture --condition relaxed --session fit-2 --label natural-refit-1 --duration 60 > natural-refit.jsonl
python3 scripts/eye_measure.py evaluate \
  --closed closed.jsonl --relaxed relaxed.jsonl --wide wide.jsonl \
  --validation-relaxed natural-1.jsonl --validation-relaxed natural-refit.jsonl \
  --margin-left 0.05 --margin-right 0.02 \
  --min-base-width 0.05 --min-wide-width 0.05
```

4数値は必須の明示比較候補で、製品既定値・採用値・自動選択ではない。評価は設定保存しない。実行前に各capture summaryの品質・中断・条件/sessionを確認する。観測coverageにはnominal producer rateを証明しない限界があり、unknownを合格扱いしない。

## 未実装と未検証

この準備段階では数値校正、見開き合成、profile測定workflowは未実装だった。現在は[まぶた仕様](../design/2026-10-03-eyelid-calibration-design.md)と[IPC仕様](../design/2026-10-06-panel-ipc-design.md)に従い実装済み。未校正時は従来raw＋filter＋最終capを維持し、自然開眼へ0.75を掛けない。

この準備段階では実producer（ABI5含む）、実測cadence/futex wake、実vrlink /proc、Steam Frame実行、HMD操作・画像受理、manifest登録/autostart、ARM compile/runtimeは未検証だった。模擬producer・SDK fake・実Unix/process・localhost UDPの成功を実機合格へ拡張しない。後続の実機確認は校正検証報告と実機調査報告に区別して記録する。
