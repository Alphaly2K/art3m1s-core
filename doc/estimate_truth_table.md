# `estimate` 真值表

本表记录原版 Artemis 引擎探针得到的行为。测试变量为：

```text
t.probe_num = 2
```

探针使用的条件没有加入 `$` 前缀：

| 条件 | 原版结果 | 观察到的含义 |
| --- | --- | --- |
| `1 == 2` | `true` | 裸参数读取开头数值 `1`，后面的文本不参与比较 |
| `t.probe_num != 2` | `false` | 开头没有数值，转换为 `0` |
| `0` | `false` | 数值 `0` |
| `1` | `true` | 数值 `1` |
| `t.probe_num` | `false` | 开头没有数值，转换为 `0` |
| `t.probe_num == 2` | `false` | 开头没有数值，转换为 `0` |
| `t.probe_num != 3` | `false` | 开头没有数值，转换为 `0` |

结论：`estimate` 带 `$` 时才进入表达式求值；不带 `$` 时按数值参数转换，读取开头的数值，无法读取数值时按 `0` 处理。比如：

```text
estimate="1 == 2"       -> 1 -> true
estimate="t.probe_num != 2" -> 0 -> false
```

这解释了：

```text
estimate="t.lydialog.fontarrign_default.size != 2"
```

原版引擎会将其转换为 `0`，所以不会执行错误提示分支。
