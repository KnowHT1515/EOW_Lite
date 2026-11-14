# Transport

`Transport` 模块是系统模块间的通信中枢。采用

> Module_A <---> Transport <---> Module_B 

的通信形式

任何模块在任何時刻都不允许绕过 `Transport` 模块与其他模块进行通信

## TCP

`EOW_Lite` 的模块间通信基于本地 `TCP`，使用 `Envelope_Lite` 进行通信，默认使用 `127.0.0.1`(IPv4) 作为地址

### TCP Transportation

`TCP` 字节流切割使用以下规范：

[ `length_prefix` : `4` bytes ] [ `body` : `length_prefix` bytes ] [ `sentinel` : `5` bytes ]

- `length_prefix`

    报文长度前缀，固定 `4` 字节，无符号整数，大端序(网络字节序)，其值为紧随其后的 `body` 段的字节数

    `length_prefix` 的值不得超过 `20` MB(即 `20` * `1024` * `1024` 字节)、不得为 `0`，否则中断 `TCP` 连接

- `body`

    报文内容物，长度为 `length_prefix` 指定的字节数，可由任意非空字节序列组成

- `sentinel`

    报文尾哨兵，固定为 `5` 字节序列 `\eow/`：`0x5C` `0x65` `0x6F` `0x77` `0x2F`

    `sentinel` 不参与边界裁定，仅作帧完整性校验。接收方读毕 `length_prefix` 指定的 `body` 字节后，继续读取固定 `5` 字节，校验其是否等于 `\eow/`。若校验失败，则中断 `TCP` 连接

### TCP Connection

`Transport` 作为服务端持续监听自身端口号，其他模块作为客户端连入 `Transport`。成功连接后，其他模块应当首先向 `Transport` 模块发送握手报文

```json
{
    "handshake": {
        "schema": "1.0",
        "module": "<module>"
    }
}
```

`Transport` 模块校验 `module` 值合法性后返回应答

```json
{
    "handshake": {
        "result": "accept" | "deny",
        "reason": null | "invalid_module" | "module_in_use" | "schema_incompatible"
    }
}
```

`result:accept` 代表握手请求通过，并将该模块注册为上线

`result:deny` 代表握手请求不通过，原因为 `<reason>`，并在返回应答后断开连接

---

## Envelope_Lite

`Envelope_Lite` 使用 `UTF-8` 编码的 `JSON` 格式，结构规范如下：

```json
{
    "envelope_schema": "1.0",
    "envelope_kind": "request" | "response" | "event",
    "source": "<module>",
    "target": "<module>",
    "event_id": "<14digit_string>",
    "correlation_id": null | "<14digit_string>",
    "ts_utc": "<iso8601_utc>",
    "payload_schema": "<namespace>@<version>",
    "payload": { ... }
}
```

### source & target

发起方模块与接收方模块，字段类型为字符串

`Transport` 模块校验其取值合法性，合法取值详见 `module.json`

### event_id

全系统事件标识字段，`14 位纯数字字符串`，由发起方在生成 `Envelope` 時填入。格式如下:

> [1-digit code] + [2-digit module] + [9-digit timestamp] + [2-digit random number]

`Transport` 模块作为路由方校验 `event_id` 值的合法性:

- 值必须为 `14` 位纯数字字符串
- 首位必须为 `7`
- 模块位必须为已注册模块的 `number`
- 時间位必须为合法時间值

`Transport` 模块不校验其唯一性，不维护编号分配

### correlation_id

当 `envelope_kind` 为 `response` 时，`correlation_id` 值为该 `response` 对应 `request` 的 `event_id` 值；否则 `correlation_id` 为 `null`

`Transport` 模块校验:

- 当 `envelope_kind == "response"` 時，该字段必须有值
- 当 `envelope_kind != "response"` 時，该字段必须为 `null`

`Transport` 模块不额外校验 `correlation_id` 值的合法性

### payload

业务内容物字段，字段类型为 `JSON` 对象

`Transport` 模块不读取、不解析、不修改该字段

---

## Response Schema

`envelope_kind:response` 的格式规范由 `Transport` 模块制定

`Response` 的 `payload` 必须遵循以下规范:

```json
{
    "ok": bool,
    "result": { ... }
}
```

> "payload_schema": "transport@1.0"
