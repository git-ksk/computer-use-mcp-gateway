# V2 typed semantic authorization

> この日本語版は [`V2_SEMANTIC_AUTHORIZATION.md`](V2_SEMANTIC_AUTHORIZATION.md) の翻訳です。**英語版をcanonicalとします。**

Status: **`0.4.0` candidate向けに#221で実装済み**。

## Purpose

CUMGは既に次のexact tupleをauthorizeします。

```text
AuthenticatedClientPrincipal -> stable device -> exact DeviceCapability
```

typed semantic authorizationは、同じnorthbound execution boundaryに**narrow-only**な第2 decisionを追加します。capabilityをgrantすることはできず、exact capability authorizationを通過したfinalized commandを追加でrejectできるだけです。

初期constraintはbackend-neutralな2種類に限定します。

- `TypeText`: exact finalized textのUTF-8 byte上限;
- `BrowserNavigate`: normalized **requested origin** allowlist。

regex / JSON expression / OPA風のgeneric escape hatchはcontractに含めません。

## Decision boundary

ordinary northbound executionは次の順です。

```text
verified principal
    -> exact principal/device/DeviceCapability authorization
    -> CUMG semantic commandのparse + resolve + normalize
    -> typed semantic constraint evaluation
    -> private AuthorizedSemanticCommand
    -> Hub admission + durable bounded decision metadata
    -> durable dispatch直前のstale snapshot fence
    -> provider materialization / Agent dispatch
```

`AuthorizedSemanticCommand`はnorthbound実装内privateで、evaluateしたexact `DeviceCommand`を所有します。ordinary northbound executionはraw `DeviceCommand`ではなくこのwrapperだけを受けるため、allow後にcaller argumentからconstrained fieldを再構築しません。

tool discoveryはadvisory-onlyです。finalized-command seamでexact capability authorizationを再確認し、semantic constraintがallowしてもexact authorizationは必須です。

## Operator policy file

optional policyは次で設定します。

```text
CUMG_V2_SEMANTIC_CONSTRAINT_POLICY_FILE=/private/path/semantic-constraints.json
```

`v2_hub`は既存trusted/private-file boundaryから読み込みます。最大64 KiBです。JSONはstrictで、unknown field、unknown rule kind、同一capabilityのduplicate rule、malformed value、revision `0`、invalid rule ID、invalid origin、protocol ceilingを超えるboundはstartup/configuration時にfail closedします。

例:

```json
{
  "revision": 12,
  "rules": [
    {
      "kind": "type_text_max_utf8_bytes",
      "rule_id": "interactive-text-small",
      "max_utf8_bytes": 4096
    },
    {
      "kind": "browser_navigate_requested_origins",
      "rule_id": "browser-prod-origins",
      "allowed_origins": [
        "https://example.com",
        "https://admin.example.com:8443"
      ]
    }
  ]
}
```

policy fileはoptionalです。未設定なら既存exact capability policyがauthorization contractのままです。設定した場合もtyped ruleを持つcapabilityだけに追加semantic ceilingを適用し、他capabilityはexact-capability authorizationのみを維持します。現在はcaller / Agent / session suppliedのsemantic policy inputを提供しません。

将来narrower deployment/session layerを追加する場合も、operator ceilingとのintersectionとしてのみ許可し、widenは禁止します。

## Snapshot identity / change

valid policyをcanonicalizeし、SHA-256 snapshot digestを生成します。Hub runtimeはexactな`(revision, digest)`を1つだけinstallします。

- exact same `(revision, digest)`の再installはidempotent;
- same revision / different contentsはreject;
- running Hubへのdifferent revision installもreject;
- policy変更にはreviewed Hub restart/revision transitionが必要;
- Agent/caller向けhot-reload/widening endpointは存在しない。

digestはreviewed policy contentのidentityであり、text/URLなどrequest valueのfingerprintではありません。

## Constraint semantics

### `TypeText` byte ceiling

exact finalized `DeviceCommand::TypeText` / `TypeTextAdvanced`のUTF-8 byte列に対して評価します。limitは既存protocol maximum以下である必要があります。allowされた文字列はcurrent Cua adapterの`text` argumentへ変更せず渡されます。別の文字列をauthorizeしてから変換後の文字列をexecuteすることはありません。

### `BrowserNavigate` requested-origin allowlist

configured originはcredential/path/query/fragmentを持たないabsolute HTTP(S) originだけを許可します。requested navigation URLをtyped URL parserでnormalizeし、serialized originをallowlistと比較します。default portも一貫してnormalizeされます。

これは**requested-origin authorization**でありredirect confinementではありません。current `NavigationCompleted` contractはrequested URL transitionをproveしますが、redirect後のfinal originをenforce/attestしません。したがってcross-origin redirectをconfineするとclaimしません。そのclaimにはredirect outcomeをpreventまたはattestできる別途review済みbackend contractが必要です。

`about:` navigationはrequested-origin ruleの対象外です。deploymentが`BrowserNavigate` origin constraintを設定した場合、soundなHTTP(S) originを持たないnavigationはunsupported semantic subjectとしてfail closedします。

## Durable audit / privacy

allowされたconstrained operationがpersistするのは次のbounded decision metadataだけです。

- policy revision;
- 64 hex文字のsnapshot digest;
- fixed constraint kind;
- bounded operator rule ID。

raw typed text、requested URL、policy content、backend/private ref、credential/token、screenshot、clipboard data、provider payloadはsemantic authorization evidenceへ保存しません。

denyされたcallはfixed `semantic_constraint_denied` categoryとbounded revision/snapshot/reason metadataだけを返します。provider dispatch前なのでexecution operationを作らず、`Indeterminate`にもなりません。

## Stale-decision fence

operation admission recordはfinalized commandをallowしたexact `(revision, digest)`へbindします。durable dispatch boundary直前にHubがactive immutable snapshotと比較します。

一致しなければnot-yet-dispatched operationを`semantic_constraint_snapshot_stale`でcancelします。

- provider dispatchなし;
- `dispatched_at`なし;
- dispatch bindingなし;
- quarantine / `Indeterminate`なし;
- original bounded decision evidenceはaudit用に維持。

production runtimeにはsnapshot hot reload自体がありませんが、stale fenceもdefense-in-depthとして実装し、forced-divergence testで検証します。

## Durable schema

#221でexecution-safety durable schemaをv11から**v12**へ上げ、semantic-constraint admission evidenceをactive/terminal operation stateとbounded recovery archiveへpersistします。

semantic evidenceを持たない旧snapshotは引き続きmigrateできます。一方semantic evidenceを含むv12 snapshotはv11以前へdowngradeできません。どのreviewed snapshotがcommandをauthorizeしたかを示すadmission recordを黙って失うためです。

## Security boundary / non-claims

semantic constraintがnarrowするのは、uncompromised Hubにauthenticated callerがadmitさせられる範囲です。次のものにはなりません。

- exact `DeviceCapability` authorizationの代替;
- external grant signerのargument-aware化;
- Agentによるtext/origin policyのindependent enforcement;
- fully compromised Hubの無害化;
- Handoff / local-user recovery / quarantine / no-auto-replayの代替;
- filesystem/process/browser sandbox;
- redirect confinement;
- generic policy language。

external signerはindependentなexact device/capability/TTL ceilingのままです。compromised Hubに対するindependent semantic enforcementが必要なら、別authority boundaryでsigned semantic subjectをbindする別途review済みprotocol changeが必要です。

## Evidence

automated coverageには次を含みます。

- final UTF-8 byte評価 / requested-origin normalization;
- malformed/unknown/duplicate policyのstrict rejection;
- finalized-command seamでもexact capability authorizationがmandatory;
- raw constrained valueを含まないprivacy-bounded denial;
- exact revision+digest snapshot immutability;
- forced stale snapshotをdispatch前cancelし`Indeterminate`/quarantineを作らないこと;
- execution-safety v12 persistence/migration;
- TypeText / BrowserNavigateのprovider-materialization preservation;
- existing northbound/recovery/cancellation/ambiguity/no-replay regression。
