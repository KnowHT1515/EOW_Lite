# UI

`UI` 模块是系统的前端模块，是系统与本地操作环境进行交互的承载点

## UI Panel

`UI` 模块将会尝试拉起一个 `Windows Terminal` 终端，并将其作为交互的载体，即 `UI Panel`。若不存在可用的 `Windows Terminal` 应用，将使用系统默认兜底终端

在终端显示中，底部为用户输入区，其余部分为对话历史区。使用 `↑` `↓` 或 `Page Up` `Page Down` 对对话历史进行滚动。在对话历史区中，用户输入历史将携带白色 `>` 前缀，模型输出历史将携带黄色 `<` 前缀

`UI Panel` 将禁用 `Ctrl + C` 退出方法

### Inbound

`UI Panel` 中的单轮输入 `<input>`，将被序列化为字符串 `"<input>"` 构造:

```json
{   // CORE_API::core.llm_inbound
    "operation": "core.llm_inbound",
    "private": false,
    "permanent": true,
    "inbound": {                
        "context_id": "<14digit_string>",
        "peer_type": "human",               // UI 模块构造的请求中固定为 human
        "peer_kind": "external",            // UI 模块构造的请求中固定为 external
        "peer_name": "anonymous",           // UI 模块构造的请求中固定为 anonymous
        "time": "<iso8601_utc>",
        "silent": false,                    // UI 模块构造的请求中固定为 false
        "content": "<input>"                // 原始输入 input
    }
}
```

并向 `Core` 模块(`target`:`core`)发送请求

### Outbound

`UI Panel` 将主动渲染打印 `core.llm_context` 中 `context_type:outbound` 且 `content_type:text` 且 `private:false` 的 `content` 值(以下简称为 `output_text`)；忽略 `content_type:tag` 或 `private:true` 的 `content` 值；并以 `context_type:outbound_end` 的 `core.llm_context` 事件(以下简称为 `output_end`)为一轮输出的结束标识

`UI` 模块接收到的对话输出内容已被 `Core` 模块提前切分为若干 `chunk`，但 `UI Panel` 不整块打印 `chunk`，而是以字符为单位，使用 `10~20 ms` 的随机间隔**逐字符**打印对话输出

`UI Panel` 内置 `Render Queue` 队列，将一轮输出接收到的所有 `output_text` 依照 `chunk_id` 顺序排列，新接收到的 `output_text` 自然追加进入 `Render Queue` 并排序，接收到 `output_end` 后，视为本轮 `Render Queue` 队列完成，将所有剩余 `output_text` 全部打印完毕后重新刷新一次 `Render Queue` 中记录的完整对话输出以确保无遗漏，随后清空 `Render Queue` 并准备接收新的对话输出。`Render Queue` 以 `context_id` 为单位，相同 `context_id` 的所有 `output_text` 与 `output_end` 全部归属于同一 `Render Queue`

若依次传入的 `chunk_id` 出现乱序现象，`Render Queue` 将会自动按照 `chunk_id` 重新排列

若依次传入的 `chunk_id` 出现跳序、缺序现象，`Render Queue` 将会给予 `5` 秒等待窗口以等待缺失的 `chunk_id`，否则将关闭该 `Render Queue`，并于已打印的文本之后追加打印固定文本 ` *Outbound Missing`