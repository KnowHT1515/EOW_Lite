# Core

`Core` 模块为 `Echoes of the White` 中部分逻辑的轻量实现版，可用于各类简单测试

## Config

`Core` 模块通过 `.eow_lite_config_root` `Marker` 文件校验本地配置文件根目录，基于此搜索、读取、解析并应用各类本地配置文件

`Core` 模块将递归扫描配置文件目录下的所有文件，并收集 `.toml`, `.json`, `.md` 文件。随后根据自身逻辑读取、解析并应用指定的配置文件

---

## LLM

`Core` 模块读取本地 `core.toml` 配置文件，通过 `[LLM_Model]` 获取 `LLM` 模型配置信息，并通过 `[LLM_Prompt]` 组装对话 `prompt`，最后依照 `[SentenceSplitter]` 处理模型输出

### Inbound

对于各 `SLOT` 的 `HTTP` 请求，以下为一个简单请求 `JSON` 结构体：

```json
{
    "model": "<model>",
    "messages": [
    {
        "role": "system",
        "content": "<prompt>"
    },
    {
        "role": "user",
        "content": "<inbound>"
    },
    {
        "role": "assistant",
        "content": "<outbound>"
    },
    {
        "role": "user",
        "content": "<inbound>"
    }
  ],
}
```

接收 `text.inbound` 请求后，`Core` 模块将 `LLM` 模型的 `<inbound>` 输入文本结构化为以下形态：

```json
{
    "peer_type": "human",                                                         // 身份类
    "peer_kind": "principal",                                                     // 身份组
    "peer_name": "KnowHT",                                                        // 身份信息
    "time": "2026-07-15T12:00:00Z",
    "silent": false,                                                              // 本次输入是否强制要求静默，若 silent:true 将强制模型返回 <k.action:silent/>。
    "content": "Someone tell Vedal there is a problem with Neuro's AI."           // inbound 输入内容，格式为纯文本
}
```

随后转换为紧凑 `JSON`：

```json
{"peer_type":"human","peer_kind":"principal","peer_name":"KnowHT","time":"2026-07-15T12:00:00Z","silent":false,"content":"Someone tell Vedal there is a problem with Neuro's AI."}
```

并向 `LLM` 模型发送请求。同步发布事件 `core.llm_context`，事件 `payload` 为

```json
{   
    "topic": "core.llm_context",
    "data": {
        "context_type": "inbound",
        "context_id": "<14digit_string>",
        "content": "<content>"
    }
}
```

其中 `context_id` 的值为该轮 `Inbound` 请求传入时生成的 `event_id`，一轮完整的 `Inbound` + `Outbound` 中所有相关事件的 `context_id` 恒相同

### Outbound

对于 `LLM` 模型的 `HTTP` 响应，以下为一个简单的非流式响应 `JSON` 结构体：

```json
{
    "choices": [
    {
        "index": 0,
        "message": {
            "role": "assistant",
            "content": "<outbound>"
        },
        "finish_reason": "stop"
    }
  ]
}
```

对于流式响应，模型不会一次性返回完整的 `<outbound>`，而是通过多个连续的流式响应 `frame` 片段逐步返回新增文本

单个流式响应 `frame` 片段的 `JSON` 结构体：

```json
{ 
    "choices": [{ 
            "index": 0, 
            "delta": { 
                "content": "<frame>" 
            }, 
            "finish_reason": null 
        }
    ] 
}
```

其中，`<frame>` 表示本次响应新增的文本内容。多个 `<frame>` 按照接收顺序拼接后，构成完整的 `<outbound>`

流式响应结束时，核心模型将返回一个不再包含新增文本、且 `finish_reason` 不为 `null` 的结束片段，例如：

```json
{ 
    "choices": [{ 
            "index": 0, 
            "delta": {}, 
            "finish_reason": "stop" 
        } 
    ] 
}
```

`LLM` 模型的 `<outbound>` 输出文本使用自然语言与 `Tag` 方法，不强制要求格式化输出。输出文本将由 `Sentence Splitter` 切句器进行切分成为多个 `chunk` 以便于后续行为

在 `LLM` 模型的 `<inbound>` 输入文本中，若 `silent:true`，则 `LLM` 模型的 `<outbound>` 输出文本恒为 `<k.action:silent/>`，即 `Tag` 方法设定的 `静默` 标签。此时无需进行真实模型请求，而是选择在 `Core` 模块内部直接拼装 `<inbound>` 与对应的 `<outbound>`(`<k.action:silent/>`) 作为 `context`，其与真实请求的结果无异，且实现更简单，负载更低

对于 `<outbound>` 被 `Sentence Splitter` 切分出的多个 `chunk`，`Core` 模块将发布 `context_type:outbound` 的 `core.llm_context` 事件。每轮 `Event` 事件发布或 `Request` 请求仅对应一个 `chunk`

事件 `payload` 为:

```json
{
    "topic": "core.llm_context",
    "data": {
        "context_type": "outbound",
        "context_id": "<14digit_string>",
        "chunk_id": <int>,
        "content_type": "<content_type>",
        "content": "<content>"
    }
}
```

完成本轮 `Outbound` 所有 `chunk` 的切句与发布后，`Core` 模块将发布 `context_type:outbound_end` 的 `core.llm_context` 事件

对于 `context_type:outbound_end` 的 `core.llm_context` 事件，其发布时机**必须**为所有 `context_type:outbound` 的 `core.llm_context` 事件完成发布**之后**

事件 `payload` 为:

```json
{
    "topic": "core.llm_context",
    "data": {
        "context_type": "outbound_end",
        "context_id": "<14digit_string>"
    }
}
```

其中 `context_id` 的值为该轮 `Outbound` 所对应 `Inbound` 请求传入时生成的 `event_id`，一轮完整的 `Inbound` + `Outbound` 中所有相关事件的 `context_id` 恒相同

### Context

当前每个实例维护一份最多 `10` 轮的进程内对话历史。每轮包含输入和输出，超过上限时按完成顺序淘汰最早的记录。

`permanent: false` 的请求可读取已有历史，完成后跳过整轮历史提交。

---

## Sentence Splitter

`Core` 模块采用 `Sentence Splitter` 对 `LLM` 模型的 `outbound` 按照指定逻辑切分为多个 `chunk`，原则上一个 `chunk` = 不影响可识别性与语义理解的最小语句

对于流式与非流式 `outbound` 均采用同一 `Sentence Splitter` 逻辑

每个 `chunk` 使用以下格式进行组织:

```json
{"chunk_id":<int>,"content_type":"<content_type>","content":"<content>"}
```

其中 `<content_type>` 的合法枚举值为 [`tag`, `text`]，分别对应常规文本切分与 `Tag` 方法切分

`chunk` 切分将会原样保留首尾空白，不做处理，特殊规则例外

如果在切分中出现**无实际内容**的 `chunk`，如仅空格、仅换行、完全空白等，将完全丢弃该 `chunk`，不计入 `chunk_id`

当前 `Core` 模块使用简单规则切句器，切句器配置存储于本地 `Config` 文件中

下方是一份 `Sentence Splitter` 配置样例

```toml
# 以下内容存放于 config/core.toml 文件下
[SentenceSplitter]
hard_split_tokens = [                                                      # 硬切分字符串集合
    { tokens = [".", "?", "!"], placement = "previous" },                  # 切分后被归类于上一句(句末)
    { tokens = [], placement = "discard"},                                 # 切分后被丢弃                 
    { tokens = [], placement = "next" },                                   # 切分后被归类于下一句(句首)
]
soft_split_tokens = [                                                      # 软切分字符串集合
    { tokens = [",", ";", ":", "—", ")"], placement = "previous" },        # 切分后被归类于上一句(句末)
    { tokens = [], placement = "discard"},                                 # 切分后被丢弃
    { tokens = ["("], placement = "next" },                                # 切分后被归类于下一句(句首)
]
white_list = []               # 切分白名单集合
tag_begin_tokens = "<k."      # Tag 方法起始标识
tag_end_tokens = "/>"         # Tag 方法结束标识
buffer_chars = 15             # 滑动缓冲窗口长度
target_chars = 180            # 单 chunk 最小软切分触发阈值
max_chars = 240               # 单 chunk 最大切分触发阈值
min_chars = 60                # 单 chunk 最小硬切分触发阈值
```

其中：

- `chars` 的计数规范统一为**码点**数
- `hard_split_tokens` 为硬切分字符串集合，当本轮切分字符计数触及(>=) `min_chars` 后，若出现任一集合中的元素，则以该元素为界将已积累的字符切分为 `chunk` 送出
- `soft_split_tokens` 为软切分字符串集合，当本轮切分字符计数触及(>=) `target_chars` 后，若出现任一集合中的元素，则以该元素为界将已积累的字符切分为 `chunk` 送出
- `tokens` 为切分标识的字符串集合
- `placement` 的合法枚举为 [`previous`, `discard`, `next`]，表示在执行切分行为后对相应集合元素的处理方法，分别为 [归类于上一句(句末), 丢弃, 归类于下一句(句首)]
- `white_list` 为白名单字符串集合，使用**精确字符串匹配**，`HardSplit` 与 `SoftSplit` 切分规则对白名单内容不生效
- `tag_begin_tokens` 与 `tag_end_tokens` 分别为 `Tag` 方法的起始标识与结束标识。对于 `Tag` 方法的识别与切分规则：
    - `Tag` 方法的切分规则不受 `min_chars` 的字符数限制，不受 `hard_split_tokens` 与 `soft_split_tokens` 的规则影响
    - 当识别到 `tag_begin_tokens` 标识后，将起始标识前的内容切分为 `chunk` 送出，并开启一轮 `Tag` 方法切分
    - 当识别到 `tag_end_tokens` 标识后，从 `tag_begin_tokens` 标识到 `tag_end_tokens` 标识，将包含两端标识的完整内容作为独立 `chunk` 切分后送出
    - `Tag` 方法的切分规则仍受 `max_chars` 的字符数限制，若触及 `max_chars` 阈值且向文本末尾方向搜寻 `buffer_chars` 个字符后仍未闭合，则将已累计内容（包括 `tag_begin_tokens`）作为普通文本处理，重新启用常规切分规则尝试进行切分
    - 完成 `Tag` 方法的切分后，恢复常规切分规则
- `buffer_chars` 为切分器滑动缓冲窗口长度：
    - 当切分标识进入缓冲窗口后，预备执行切分；当切分标识离开缓冲窗口后，对切分标识前的内容执行切分并送出 `chunk`：
        - 当 `split_tokens` 进入缓冲窗口后，预备执行切分，待 `split_tokens` 离开缓冲窗口，对 `split_tokens` 前的内容执行切分并送出 `chunk`
        - 当 `tag_begin_tokens` 进入缓冲窗口后，预备执行切分，待 `tag_begin_tokens` 离开缓冲窗口，对 `tag_begin_tokens` 前的内容执行切分并送出 `chunk`
        - 当 `tag_end_tokens` 进入缓冲窗口后，预备执行切分，待 `tag_end_tokens` 离开缓冲窗口，对 `tag_end_tokens` 前的内容执行切分并送出 `chunk`
    - 缓冲窗口也具备其他用途：
        - 当 `hard_split_tokens` 之后**紧接**着其他 `hard_split_tokens`，以最尾端的 `hard_split_tokens` 为准执行切分，将之前的 `hard_split_tokens` 视为普通文本保留
        - 当 `hard_split_tokens` 之后**紧接**着之后紧接非空白、非换行且非其他 `hard_split_tokens` 的字符，则忽略该 `hard_split_tokens`，不执行切分操作
    - 当缓冲窗口完整遍历所有内容后，结算已积累的内容与缓冲窗口内的剩余字符，此时创建的新 `chunk` 不受 `min_chars` 的字符数限制：
        - 存在切分标识的：依照既有切分规则将内容切分为多段 `chunk`，在既有切分规则下无法进行切分的剩余内容再作为独立的一段 `chunk`
        - 不存在切分标识的：将所有内容合并为一段 `chunk`
- 对于 `max_chars`，当累计字符数触及(>=) `max_chars`，且前 `max_chars` 个字符不存在任何可用的切分标识，以第 `max_chars` 个字符之后的边界作为 `触及点`：
    - 首先从 `触及点` 向文本末尾方向搜寻 `buffer_chars` 个字符，寻找空格、换行符、`hard_split_tokens` 标识与 `soft_split_tokens` 标识、`Tag` 方法标识:
        - 若存在 `Tag` 方法标识，则以该标识为准进行 `chunk` 切分，否则继续搜寻
        - 若存在 `hard_split_tokens` 标识，则以该标识为准进行 `chunk` 切分，否则继续搜寻
        - 若存在 `soft_split_tokens` 标识，则以该标识为准进行 `chunk` 切分，否则继续搜寻
        - 若存在换行符，则以该换行符为准进行 `chunk` 切分，否则继续搜寻
        - 若存在空格，则以该空格为准进行 `chunk` 切分
        - 特别地，当 `Tag` 方法的切分触及 `max_chars` 后，仅允许搜寻 `tag_end_tokens` 标识，若存在则正常闭合
    - 若向文本末尾方向的搜寻未发现任何可切分内容，则转为从 `触及点` 向文本开头方向搜寻 `buffer_chars` 个字符，寻找空格与换行符：
        - 若存在换行符，则以该换行符为准进行 `chunk` 切分，否则继续搜寻
        - 若存在空格，则以该空格为准进行 `chunk` 切分
    - 向前/向后的搜寻遵循 `Tag` > `HardSplit` > `SoftSplit` > 换行 > 空格的优先级规则，在该优先级规则下，若存在多个符合条件的相同类型的切分内容，遵循 `就近原则`，以距离 `触及点` 最近的切分内容为标准进行切分
    - 若基于空格、换行符的切分，则将该切分内容追加至上一段 `chunk` 的尾部，而非下一段 `chunk` 的头部
    - 若向前与向后的搜寻均未发现任何符合条件的可切分内容，则直接在 `触及点` 强制切分
- 理论上的单 `chunk` 最大字符数限制为 `max_chars` + `buffer_chars`

---

## Tag

通过提供系统自定义 `Tag` 方法，允许 `LLM` 模型在尽可能保持纯文本输出的前提下发送行为或操作请求

`Tag` 方法的统一格式为:

`<k.` + `TagType` + `:` + `TagId` + (`(` + `Parameters` + `)`)? + `/>`

[`TagType`, `TagId`, `Parameters`] 中均不允许包含任何在 `JSON` 的字符串值中必须进行转义的字符，也不允许包含
[正斜杠 `/`, 反斜杠 `\`, 左圆括号 `(`, 右圆括号 `)`, 左尖括号 `<`, 右尖括号 `>`]

在此规则下：

- `TagType` 使用单个全小写英文单词的命名方法
- `TagId` 使用 `snake_case` 的命名方法
- `Parameters` 的命名方法由其对应 `TagId` 决定

以下是几个 `Tag` 方法的格式示例：

- `<k.action:silent/>`
- `<k.action:play_sound(pipe)/>`
- `<k.request:get_memory/>`
- `<k.request:sync_utterance/>`
- `<k.selection:choice(0)/>`
- `<k.selection:pass/>`
- `<k.point:sync_utterance_begin/>`
- `<k.point:sync_utterance_end/>`

`Core` 模块将对 `Tag` 方法进行解析，若存在 `Core` 模块支持的 `Tag` 方法，则执行相应的行为，否则将忽略

`Core` 模块自身支持的 `Tag` 方法有:

- `<k.action:silent/>`
    `行为:静默` 标签，意为本次请求中 `LLM` 模型选择保持静默，不产出实际响应内容。不会执行任何行为

---

## CORE_API

### Request

- `core.llm_inbound`
- `core.llm_push_prompt`

#### core.llm_inbound

`payload`:

```json
{
    "operation": "text.inbound",
    "permanent": true,
    "inbound": {                
        "context_id": "<14digit_string>",   
        "peer_type": "<PeerType>",
        "peer_kind": "<PeerKind>",
        "peer_name": "<PeerName>",
        "time": "<iso8601_utc>",
        "silent": <bool>,
        "content": "<content>"
    }
}
```

#### core.llm_push_prompt

`payload`:

```json
{
    "operation": "text.push_prompt",
    "module": "<module>",
    "prompt": "<prompt>"
}
```

## Examples

由于本文档中的部分概念可能显得有些晦涩难懂，故这里集中展示了几个实际用例，方便读者进行理解

### Sentence Splitter

下方是一个实际切句器例子：

假设 `Sentence Splitter` 的配置为：

```toml
[SentenceSplitter]
hard_split_tokens = [
    { tokens = [".", "?", "!"], placement = "previous" },
    { tokens = ["\n\n"], placement = "discard" },
    { tokens = [], placement = "next" },
]
soft_split_tokens = [
    { tokens = [",", ";", ":", "—", ")"], placement = "previous" },
    { tokens = [], placement = "discard" },
    { tokens = ["("], placement = "next" },
]
white_list = ["Dr."]
tag_begin_tokens = "<k."
tag_end_tokens = "/>"
buffer_chars = 10
target_chars = 45
max_chars = 80
min_chars = 30
```

假设 `Sentence Splitter` 依次接收到以下文本：

```text
Dr. Lane checked the value 3.14 and kept moving without hesitation?! The corridor stayed narrow and strangely quiet, but everyone continued forward <k.action:play_sound(bell)/> The backup lights stayed active until the operator stopped the test.

sha256=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08_artifact001 was copied into the report for later inspection.

The parser reads many ordinary words until it reaches this nearby boundary uninterruptedidentifiercontinues. final tail
```

当所有文本均已输入并完成结算后，产生如下 `chunk`：

```json
[
    {"chunk_id":0,"content_type":"text","content":"Dr. Lane checked the value 3.14 and kept moving without hesitation?!"},
    {"chunk_id":1,"content_type":"text","content":" The corridor stayed narrow and strangely quiet,"},
    {"chunk_id":2,"content_type":"text","content":" but everyone continued forward "},
    {"chunk_id":3,"content_type":"tag","content":"<k.action:play_sound(bell)/>"},
    {"chunk_id":4,"content_type":"text","content":" The backup lights stayed active until the operator stopped the test."},
    {"chunk_id":5,"content_type":"text","content":"sha256=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08_artifact001 "},
    {"chunk_id":6,"content_type":"text","content":"was copied into the report for later inspection."},
    {"chunk_id":7,"content_type":"text","content":"The parser reads many ordinary words until it reaches this nearby boundary "},
    {"chunk_id":8,"content_type":"text","content":"uninterruptedidentifiercontinues."},
    {"chunk_id":9,"content_type":"text","content":" final tail"}
]
```

各个 `chunk` 的码点数如下：

```text
chunk 0: 68
chunk 1: 48
chunk 2: 32
chunk 3: 28
chunk 4: 69
chunk 5: 84
chunk 6: 48
chunk 7: 75
chunk 8: 33
chunk 9: 11
```

切分过程说明：

0. 处理以下内容：

   ```text
   Dr. Lane checked the value 3.14 and kept moving without hesitation?!
   ```

   `Dr.` 与 `white_list` 中的内容精确匹配，因此其中的 `.` 不执行 `HardSplit`

   `3.14` 中的 `.` 后方紧接非空白、非换行且非其他 `hard_split_tokens` 的字符 `1`，因此该 `.` 同样不执行 `HardSplit`

   文本末尾的 `?` 后方紧接另一个 `hard_split_tokens` 标识 `!`，因此切分点继续向后延伸，以最尾端的 `!` 为准

   此时已累计 `68` 个码点，满足 `68` >= `min_chars`

   因此执行硬切分。`!` 的 `placement` 为 `previous`，被保留在上一段末尾，产生：

   ```json
   {"chunk_id":0,"content_type":"text","content":"Dr. Lane checked the value 3.14 and kept moving without hesitation?!"}
   ```

1. 上一次切分后的空格不会被清理，而是进入下一轮累计。随后处理：

   ```text
    The corridor stayed narrow and strangely quiet,
   ```

   当遇到 `,` 时，本轮已经累计 `48` 个码点，满足 `48` >= `target_chars`

   因此执行软切分。`,` 的 `placement` 为 `previous`，被保留在当前段末尾，产生：

   ```json
   {"chunk_id":1,"content_type":"text","content":" The corridor stayed narrow and strangely quiet,"}
   ```

   该 `chunk` 开头的空格为上一个硬切分标识 `!` 后原有的空格

2. 继续累计以下内容：

   ```text
    but everyone continued forward 
   ```

   该段只有 `32` 个码点，尚未触及 `target_chars`，其中也没有可用的硬切分标识

   随后识别到 `tag_begin_tokens`: `<k.`

   `Tag` 方法的切分不受 `min_chars` 限制，因此立即将起始标识之前已经累计的内容切分并送出：

   ```json
   {"chunk_id":2,"content_type":"text","content":" but everyone continued forward "}
   ```

   该 `chunk` 首尾的空格均被原样保留。

3. 进入 `Tag` 方法切分状态后，继续累计至识别到 `tag_end_tokens`：

   ```text
   <k.action:play_sound(bell)/>
   ```

   该 `Tag` 方法已经在 `max_chars` 以内正常闭合，因此将包含起始标识和结束标识的完整内容作为独立 `chunk` 送出：

   ```json
   {"chunk_id":3,"content_type":"tag","content":"<k.action:play_sound(bell)/>"}
   ```

   `Tag` 方法内部出现的 `:`、`(` 和 `)` 均不触发常规软切分规则

4. 完成 `Tag` 方法切分后恢复常规切分，继续处理：

   ```text
   The backup lights stayed active until the operator stopped the test.\n\n
   ```

   文本末尾的 `.` 后方紧接另一个 `hard_split_tokens` 标识 `\n\n`，因此切分点继续向后延伸，以最尾端的 `\n\n` 为准

   因此执行硬切分，`\n\n` 的 `placement` 为 `discard`，被丢弃，产生：

   ```json
   {"chunk_id":4,"content_type":"text","content":" The backup lights stayed active until the operator stopped the test."}
   ```

5. 继续处理以下连续标识符：

   ```text
   sha256=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08_artifact001
   ```

   该标识符共有 `83` 个码点，其中不存在常规切分标识

   当累计内容达到 `80` 个码点时触及 `max_chars` = `80`

   此时以第 `80` 个码点之后的边界作为 `触及点`，向文本末尾方向搜寻 `10` 个码点

   后续内容依次为：

   ```text
   001 was co
   ```

   搜寻窗口中不存在 `Tag` 方法标识、硬切分标识、软切分标识或换行符，但在完整标识符结束后发现了一个空格。该空格位于本轮内容的第 `84` 个码点，仍处于向后搜寻窗口以内

   因此以该空格为界切分，并按照空格切分规则将空格归入上一段，产生：

   ```json
   {"chunk_id":5,"content_type":"text","content":"sha256=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08_artifact001 "}
   ```

   该 `chunk` 共 `84` 个码点，未超过理论最大限制 `max_chars` + `buffer_chars` = `90`

6. 空格已经被归入 `chunk 5`，因此下一轮直接从以下内容开始：

   ```text
   was copied into the report for later inspection.\n\n
   ```

   文本末尾的 `.` 后方紧接另一个 `hard_split_tokens` 标识 `\n\n`，因此切分点继续向后延伸，以最尾端的 `\n\n` 为准

   因此执行硬切分，`\n\n` 的 `placement` 为 `discard`，被丢弃，产生：

   ```json
   {"chunk_id":6,"content_type":"text","content":"was copied into the report for later inspection."}
   ```

7. 继续处理：

   ```text
   The parser reads many ordinary words until it reaches this nearby boundary uninterruptedidentifiercontinues.
   ```

   前 `80` 个码点中不存在常规硬切分、软切分或 `Tag` 方法标识，因此在第 `80` 个码点之后触及 `max_chars` 切分逻辑

   此时 `触及点` 位于：

   ```text
   uninterruptedidentifiercontinues
        ^
   ```

   即已经读取了连续标识符开头的 `5` 个码点。

   从 `触及点` 向文本末尾方向搜寻 `10` 个码点时，后续内容仍全部属于同一个连续标识符，不存在空格、换行符或其他可切分内容

   切分器因此转为向文本开头方向搜寻 `10` 个码点，并在距离 `触及点` `5` 个码点的位置找到连续标识符前的空格：

   ```text
   ...this nearby boundary uninterruptedidentifiercontinues.
                           ^
   ```

   该空格是向前窗口内距离 `触及点` 最近的可用边界，因此在此执行切分。空格被归入上一段，产生：

   ```json
   {"chunk_id":7,"content_type":"text","content":"The parser reads many ordinary words until it reaches this nearby boundary "}
   ```

8. 向前搜寻切分后，剩余内容为：

   ```text
   uninterruptedidentifiercontinues.
   ```

   该段共有 `33` 个码点。当末尾的 `.` 被确认时，累计长度已经满足 `min_chars`，因此执行硬切分：

   ```json
   {"chunk_id":8,"content_type":"text","content":"uninterruptedidentifiercontinues."}
   ```

9. 最后剩余：

   ```text
    final tail
   ```

   该段只有 `11` 个码点，不满足 `min_chars`，也不存在任何切分标识

   当缓冲窗口已经完整遍历所有输入内容后，切分器进入最终结算。最终结算产生的新 `chunk` 不受 `min_chars` 限制，因此将全部剩余内容合并为一段：

   ```json
   {"chunk_id":9,"content_type":"text","content":" final tail"}
   ```

   该 `chunk` 开头的空格来自前一个句号后的原始输入，切分器不会主动将其清除
