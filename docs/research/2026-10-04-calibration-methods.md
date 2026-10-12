# まぶたキャリブレーション方式の調査

調査日: 2026-10-04 JST。採用判断のための一次コード調査であり、本プロジェクトの測定時間・代表値・合格閾値を確定する仕様ではない。既製ヘッドセットの内部実装を網羅した調査でもない。

## 比較

| 参考実装 | ユーザー操作・時間 | 代表値・合格基準 | 揺れ・見開きへの対処 |
| --- | --- | --- | --- |
| sasaken 手動 Eye fit | 中央・上・下・左・右の5点と閉眼。視線各点は待機0.5秒＋取得2秒（先頭0.3秒除外、実効1.7秒）。閉眼は予告3秒＋取得3秒（先頭0.5秒除外、実効2.5秒） | 左右 openness の算術平均。必要標本数は `min(45, max(12, ceil(0.6 * received)))`。視線 spread は0.06以下。閉眼は両眼とも中央開眼平均の70%未満、開閉幅0.1以上。各段最大3回 | 視線収集は両眼 openness 0.3以上・少なくとも片眼の視線が reliable。閉眼はこの開眼条件を外す。見開き測定段階はない |
| sasaken 自動開眼学習 | 開始後20秒待機、その後有効標本の減衰重み900を蓄積。90 Hzで約10秒相当だが、実時間10秒の保証ではない | 左右別、幅0.005の減衰ヒストグラムの重み付き中央値。現在の自然開眼推定の75%を超え、1未満の値だけ追加。倍率0.75..1.33 | 半減期は90 Hz前提の10分相当。閉眼基準や見開き終点を測る方式ではない。飽和値1は学習対象外 |
| Baballonia 手動 Calibration | 現在の推論値を見ながら左右の Lower/Upper スライダーを調整。固定の取得時間なし | 入力レンジを直接保存し線形変換。標本集計による平均・中央値や自動合否はこの経路にない | レンジ調整だけでは時間的な揺れを除去しない。別系統に One Euro filter がある。通常眼 producer の独立Widen生成・送信は無効 |
| Babble 眼画像モデル再訓練 | 固定した点を見ながら頭を動かす視線収集、その後約20秒閉眼。画像で個人用NNを訓練 | まぶたの raw 値から代表値を取るレンジ補正ではない。公式記事の訓練所要時間は5–10分 | 固定Baballonia標準routineでは見開き・squint収集はコメントアウト。閉眼20秒をレンジ補正の推奨時間と解釈できない |

## 固定コードの根拠

sasaken は `af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d` を対象とした。[panel/src/gaze_fit.h L15–78](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/panel/src/gaze_fit.h#L15-L78) に段階・時間・判定定数、[gaze_fit.cpp L180–228](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/panel/src/gaze_fit.cpp#L180-L228) に標本数と閉眼・spread判定がある。[src/capture.rs L15–25](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/capture.rs#L15-L25) の開眼条件と [L99–153](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/capture.rs#L99-L153) の有限値選別・和/標本数から、手動代表値は中央値ではなく算術平均と確認できる。spread は視線の標準偏差であり、まぶたの揺れを直接判定する条件ではない。

自動学習は [src/main.rs L110–122](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/main.rs#L110-L122) と [L711–742](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/main.rs#L711-L742)。サンプル単位で減衰し、累積重みが半分になるbinの中心を自然開眼値にする。可変取得レートではウォームアップと半減期の実時間が変わる。見開きは別処理で、[L68–91](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/main.rs#L68-L91) に low/normal/high の開始・終点差分と持続判定がある。今回の仕様へこれらの定数を採用したものではない。

Baballonia は `63ea0c21effd5bd717d150c59d6fd7d0b548af2f` を対象とした。[CalibrationView.axaml L66–89](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Views/CalibrationView.axaml#L66-L89) は0..1、刻み0.01の入力上下限と現在値表示を持つ。[CalibrationService.cs L89–108](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Services/CalibrationService.cs#L89-L108) は設定を直接保存し、[ParameterSenderService.cs L143–180](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia/Services/ParameterSenderService.cs#L143-L180) は送信前に適用する。通常眼OSC経路の変換直後にはclampがなく、native経路は左右平均をclampしてEyesClosedAmountにする。これを今回の左右独立出力の根拠と混同しない。

画像訓練用の標準routineは [ICalibrationRoutine.cs L343–375](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia.Desktop/Calibration/ICalibrationRoutine.cs#L343-L375) に視線・閉眼の画像収集、画像bin結合、trainer呼出しを持つ。閉眼は20秒、lidラベル0。標準視線取得の既定は [L162–173](https://github.com/pikepikeid/Baballonia/blob/63ea0c21effd5bd717d150c59d6fd7d0b548af2f/src/Baballonia.Desktop/Calibration/ICalibrationRoutine.cs#L162-L173) の120秒。quickは視線10秒、閉眼20秒、BlinkOnlyも閉眼20秒で、標準routineと同じ所要時間とは限らない。

[Babble公式記事](https://docs.babble.diy/blog/eballs) は、固定点を見て頭を動かす操作、約20秒の閉眼、5–10分のNN訓練を説明する。[Project-Babble/BabbleCalibration](https://github.com/Project-Babble/BabbleCalibration/tree/3685e1602dfe4eef8c764bc039da63c593f2a092) はそのGodot overlay。確認した [MainScene.cs L202–222](https://github.com/Project-Babble/BabbleCalibration/blob/3685e1602dfe4eef8c764bc039da63c593f2a092/Scripts/MainScene.cs#L202-L222) はblink等の名前付きtimer routineを外部指定時間で開始する。overlayに見開き表示routineが存在しても、上記固定Baballoniaの標準手順で見開き画像を収集する証拠にはならない。

## 採用判断へ持ち帰る点

まぶたの区間補正、時間的な揺れの平滑化、自然開眼基準の自動追従、画像モデルの再訓練は別の処理である。本体では各段階の手動開始・準備2秒・計測5秒・有効サンプル率80%以上・中央値、左右別の自然開眼最大値＋追加余裕による見開き開始を採用した（[まぶた仕様](../design/2026-10-03-eyelid-calibration-design.md)、[IPC仕様](../design/2026-10-06-panel-ipc-design.md)）。変更可能な既定値は余裕各0.10、最小基礎幅0.10、最小見開き幅0.05。参考実装の値をそのまま採用したものではない。

## PICO・Quest Pro の公開情報

追加調査日: 2026-10-04 JST。「PICO」の機種が未特定なので、Neo 3 Pro Eye と PICO 4 Pro を区別する。

| 製品 | 一次資料で確認できたこと | まぶた補正について確認できないこと |
| --- | --- | --- |
| PICO Neo 3 Pro Eye | [Tobiiの製品仕様](https://www.tobii.com/products/integration/xr-headsets/device-integrations/pico-neo-3-pro-eye) は標準5点キャリブレーションを明記 | 閉眼・自然開眼・見開きの収集秒数、代表値、合格閾値、自動追従条件。視線5点をまぶた測定手順とは扱えない |
| PICO 4 Pro | [公式EyeTrackingDemo README](https://github.com/picoxr/EyeTrackingDemo/blob/5415d5b5f448c757f404dee1e8b9b64115f1f9d4/README.md) のUser Calibrationは右コントローラーtriggerによる視線offset調整。Avatar例は左右のopennessをSDKから取得。[Project Validation](https://developer.picoxr.com/document/unity/project-validation/) はEyeTrackingCalibration設定チェックを説明 | 製品内部のまぶたキャリブレーション計算法・時間・品質判定。サンプルの視線offset調整と本体の眼開き補正は別 |
| Quest Pro | [MetaのEye Tracking Privacy Notice](https://www.meta.com/legal/quest/eye-tracking-privacy-notice/) は校正完了後に左右の数値補正係数を保存して視線方向の推定精度を上げると説明。校正なしでも稼働する。[Eyes Technology](https://developers.meta.com/vr/design/eyes-technology/) は視線校正をsystem-levelとし、APIでの開始・状態・変更を公開していない | 使用中にまぶたを自動補正する際の秒数・分位値・学習条件。公開情報で見つからないことを、内部機能が存在しない証明とは扱わない |

Questの公式noticeはfit adjustment・natural facial expressionsを視線追跡と別機能として扱う。[Movement OpenXR API](https://developers.meta.com/vr/documentation/native/android/move-api/) が表情weightsや左右EyesClosedを提供することも、内部のまぶた校正方式を公開した証拠にはならない。公開一次資料の範囲では、PICOとQuest Proのまぶた校正の優劣や完全自動補正の有無は確認できなかった。Babbleの個人用画像モデル訓練の負担と、既製センサーの視線校正・数値出力補正の負担を直接同一視しない。
