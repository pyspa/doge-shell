# Sign in with ChatGPT subscription

公式SIWCのopen-source public-client flowを使う明示provider。API-key経路が既定。
既存Codex tokenの読取り/import、他アプリのclient ID、非公開backend-apiは使わない。

## 操作

```sh
(vset "AI_CHAT_PROVIDER" "chatgpt_subscription")
chat_auth login
chat_auth status
chat_auth models
chat_model <catalog-slug>
! hello
chat_auth logout
```

`vset`は上記のLisp形式でshell変数を設定する。subscription modelには既定がない。
`chat_auth login --new` は別account/workspaceを登録する。`status` のUUID labelはregistration単位で、
`chat_auth account <label>` で既存登録を選ぶ。同じemailで登録を統合しない。
再loginはissued client IDと安定host IDを再利用する。logoutは処理中リクエストを停止し、
refresh tokenをrevokeする。remote revoke未確認時はその旨を表示し、local tokenだけを削除する。
登録とhost IDは残す。認証URL/token/opaque reasoningはログやdoctorに表示しない。

モデルcatalogの掲載順とdisplay nameを表示し、slugを送信する。掲載はentitlement保証ではない。
要約後もprovider/model/account識別を保持し、切替中の送信を取り消す。識別のない旧履歴の移行と、provider/model/accountを変更した履歴は自動流用せず `chat_reset` を案内する。
権限不足・未選択model・再認証必要状態はAPIキーへfallbackしない。
usage limit時は [ChatGPT Settings → Usage](https://chatgpt.com/settings/usage) を案内し、reset時刻を推測しない。
非対象accountの `user_not_eligible` はloginを繰り返して解決しない。

## 認証と保存

127.0.0.1のランダムportで `/auth/callback` を先にbindし、毎試行fresh state/nonce/PKCE S256。
stateをcode/errorより先に検証する。dynamic登録のissued client IDを必須とし、再loginでの変更を拒否。
exact redirect URIとresourceをtoken交換へ送り、client secretは送らない。
OIDC discovery/JWKSでRS256 signature・issuer・audience・expiry・nonceとsubjectを検証する。
必要scopeも検証し、identityだけではreadyにしない。

認証は `dogesh/subscription-auth/` のprivate directory 0700/files 0600へ保存する。
registrationとcredentialsを分離し、symlink/hardlink拒否、atomic rename、cross-process file lockを使用。
refreshはlock下でreload→更新→rotated tokenをまとめて保存する。scopeをrefresh requestへ送らない。
`expires_in`/`earliest_refresh_at`を尊重し、terminal token失効時はregistrationを残して再認証を案内する。
通常chat sessionにcredentialを載せない。Authorizationをredirect先へ転送しない。

## Responsesと既存consumer

HTTPは `store:false`, `stream:true`, 完全なinput arrayを使い、systemをdeveloperへ変換する。
`previous_response_id`、sampling/output limit、background/conversation/metadata等の非対応fieldは送らない。
stream無効のcallerも内部ではSSEを集約する。response.completedのみ成功。
failed/incomplete/refusal/terminalなしEOFは部分表示後でも失敗。visible delta後の自動retryはしない。
取消はHTTP requestをdropするが、既に使われたusageがゼロになる保証はない。terminal usageが無いattemptとcached countの欠落はunknownとして記録・表示する。

consumer向けchat-completions形式のviewと、assistant message内の `_dsh_responses` continuationを区別する。
原順序のresponse.output（encrypted reasoning、namespaced function call、message）を保持し、
既存sessionのserialization/clone/compactionで保つ。opaque encrypted contentは解釈・復号・表示しない。
function結果はcall_idを保ったfunction_call_outputとして原outputの後に送る。
namespace/nameをそのrequestの登録済みlocaltoolsと照合し、unknown/missing/duplicate IDや不正JSONを拒否。
hosted MCP/tool_search/image/computer/Code Interpreter等は送らない。既存local `tool_search` は普通の関数。

namespace内の各functionは **明示strict:false**。既存optional引数と任意MCP JSON schemaを変更しない。
省略するとResponsesがstrict正規化し得るため、strictを省略しない。既存handlerの引数検証と既定値を維持する。
structured outputはjson_schemaをtext.formatへflattenし、json_objectにはJSON developer instructionを追加する。
formatを黙ってdropしない。JSON parseはcompleted後のみ。

## 検証の限界と一次資料

この変更ではmock-onlyで検証する。実ユーザーlogin・実model inference・課金は未試験。
一般Responses仕様のtool/structured output対応は、SIWCで各実modelが成功する保証ではない。

- [SIWC overview](https://developers.openai.com/siwc/token-sharing-open-source)
- [Registration/sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)
- [Accounts/sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions)
- [Models/inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)
- [Preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)
- [Strict mode](https://developers.openai.com/api/docs/guides/function-calling#strict-mode)
- [Preserve reasoning](https://developers.openai.com/api/docs/guides/reasoning#preserve-reasoning-without-stored-responses)
