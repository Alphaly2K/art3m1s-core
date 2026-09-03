# ASB Interpreter API 文档

## 概述

ASB Interpreter 是一个用于解释执行 Artemis Engine 脚本（ASB 格式）的 Rust 库。它提供完整的脚本解析、执行和事件回调机制，适用于视觉小说引擎的脚本解释层。

**版本**: 0.1.0  
**测试状态**: ✅ 40/40 测试通过

---

## 核心特性

### 脚本解析
- ✅ ASB 二进制格式解密（通过 asb-decrypt）
- ✅ 文本格式直接解析（.iet, .ast）
- ✅ 自动格式检测（根据魔数 `ASB\0`）
- ✅ 多标签单行支持：`[tag1][tag2][tag3]`
- ✅ Lua 代码块：`[lua]...[/lua]`
- ✅ 标签定义：`*label_name`

### 表达式求值
- ✅ 算术运算：`+`, `-`, `*`, `/`, `%`
- ✅ 比较运算：`==`, `!=`, `<`, `<=`, `>`, `>=`
- ✅ 逻辑运算：`&&`, `||`
- ✅ 字符串连接：`+`（字符串类型）
- ✅ 十六进制数：`0xFF`
- ✅ 变量引用：`$variable`
- ✅ 动态变量名：`$foo.(expr)`
- ✅ 字符串字面量：`'string'`
- ✅ 括号优先级：`(expr)`

### 变量系统
- ✅ 普通变量：`var`
- ✅ 全局变量：`g.var`（跨存档）
- ✅ 临时变量：`t.var`（不存档）
- ✅ 系统变量：`s.var`（引擎状态）

**var system= 变体**（17 种）：
- `var_exist` - 检查变量是否存在
- `delete` - 删除变量
- `random` - 生成随机数
- `length` - 字符串长度
- `find` - 查找子串
- `substr` - 截取子串
- `explode` - 分割字符串
- `date` - 获取日期
- `unixtime` - 获取 Unix 时间戳
- `os` - 获取操作系统
- `fullscreen` - 全屏状态
- `minimize` - 最小化状态
- `file_exists` - 检查文件存在
- `base64_encode` - Base64 编码
- `url_encode` / `url_decode` - URL 编解码
- `screen_width` / `screen_height` - 屏幕尺寸

### 标签系统
**149 个标签处理器**，按类别分类：

#### 控制流（15 个）
- `if` / `elseif` / `else` / `/if` - 条件分支
- `loop` / `/loop` - 循环
- `jump` - 跳转
- `call` / `return` - 调用/返回
- `stop` - 停止执行
- `wt` / `wt0` - 等待点击
- `wait` - 等待指定时间
- `exkey` - 按键等待
- `@` - 点击等待标记
- `repeatedly` - 重复执行标记
- `autoskip_disable` - 禁用自动跳过

#### 剧情脚本（15 个）
- `print` - 显示文本
- `rt` - 换行
- `rp` - 分页
- `font` / `font_close` / `fontdefault` / `fontinit` - 字体设置
- `ruby` / `/ruby` - 注音
- `link` / `/link` / `linkdisable` / `linkenable` - 链接
- `glyph` - 点击等待图标
- `chgmsg` / `chgmsg_close` - 消息层切换
- `scetween` - 文本动画
- `scein` / `sceout` - 场景进入/退出
- `automode` - 自动模式
- `skip` - 跳过设置
- `backlog` - 历史记录
- `hide` - 隐藏模式
- `alreadyread` - 已读判定
- `writebacklog` - 写入历史
- `indent` - 缩进
- `prohibit` - 禁则处理
- `wordparts` - 单词部分

#### 音频（14 个）
- `splay` / `sstop` / `sfade` / `span` / `sxfade` - BGM 控制
- `seplay` / `sestop` / `sefade` / `sepan` - SE 控制
- `voice` - 语音播放
- `setonsoundfinish` / `delonsoundfinish` - 音效完成事件
- `sefadein` / `sefadeout` / `sfadein` / `sfadeout` - 淡入淡出（已弃用）
- `se_saveok` / `se_loadok` / `se_exitok` - 系统音效
- `allsoundstop` - 停止所有音效

#### 图层（9 个）
- `lyc` / `lyc2` - 创建图层
- `lydel` - 删除图层
- `lyprop` - 设置图层属性
- `lyrename` - 重命名图层
- `lyedit` - 编辑图层图像
- `lydrag` - 图层拖动
- `lytween` / `lytweendel` - 图层缓动
- `tweenset` / `/tweenset` - 缓动序列
- `lyevent` - 图层事件

#### 转场/视频/截图（8 个）
- `trans` - 画面转场
- `flip` - 立即反映
- `uitrans` - UI 转场
- `video` - 视频播放
- `anime` - 帧动画
- `takess` - 截图
- `savess` - 保存截图
- `setonvideofinish` / `delonvideofinish` - 视频完成事件

#### 系统操作（17 个）
- `exec` - 执行用户操作
- `save` / `load` - 存档/读档
- `debug` / `debugprint` / `debugreload` - 调试
- `caption` - 窗口标题
- `mouse` - 鼠标设置
- `keyconfig` - 按键配置
- `file` - 文件操作
- `httpget` / `httppost` - HTTP 请求
- `openbrowser` - 打开浏览器
- `autosave` - 自动存档
- `avoid` - 紧急回避
- `vibrate` - 振动
- `statusbar` - 状态栏
- `purchase` - 应用内购买
- `callnative` - 调用原生代码
- `dialog` - 对话框
- `yesno` - 是/否选择
- `exit` - 退出
- `gotitle` - 返回标题
- `reset` - 重置

#### 事件处理器（30 个）
- `setonpush` / `delonpush` - 按键事件
- `setonautomodein` / `delonautomodein` - 自动模式开始
- `setonautomodeout` / `delonautomodeout` - 自动模式结束
- `setonbacklogin` / `delonbacklogin` - 历史开始
- `setonbacklogout` / `delonbacklogout` - 历史结束
- `setoncommandskipin` / `deloncommandskipin` - 命令跳过开始
- `setoncommandskipout` / `deloncommandskipout` - 命令跳过结束
- `setoncontrolskipin` / `deloncontrolskipin` - 控制跳过开始
- `setoncontrolskipout` / `deloncontrolskipout` - 控制跳过结束
- `setondirchg` / `delondirchg` - 方向改变
- `setonhidein` / `delonhidein` - 隐藏开始
- `setonhideout` / `delonhideout` - 隐藏结束
- `setonwindowbutton` / `delonwindowbutton` - 窗口按钮

#### 其他（41 个）
- `var` - 变量设置
- `tag` - 执行任意标签
- `calllua` - 调用 Lua 函数
- `macroadd` / `macrodel` - 宏管理
- `rclick` - 右键菜单
- `loading` / `saving` - 加载/存档状态
- `sysshow` / `syshide` / `syssave` - 系统 UI
- `loadmask` - 加载遮罩
- `alldelete` - 全部删除

---

## API 参考

### Interpreter

主解释器结构体。

```rust
pub struct Interpreter {
    // 私有字段
}
```

#### 构造

```rust
// 使用默认配置创建
let interpreter = Interpreter::new(InterpreterConfig::default());

// 使用自定义配置
let config = InterpreterConfig {
    stage_width: 1280,
    stage_height: 720,
    fps: 60,
    ..Default::default()
};
let interpreter = Interpreter::new(config);
```

#### 脚本加载

```rust
// 从文本加载
interpreter.load_script("name", "script content")?;

// 从 ASB 二进制加载
interpreter.load_asb("name", &binary_data)?;

// 智能加载（自动检测格式）
interpreter.load_file("name", &data)?;

// 设置脚本加载器（文本）
interpreter.set_script_loader(Box::new(|name| {
    let path = format!("game/{}", name);
    std::fs::read_to_string(&path).map_err(|e| e.into())
}));

// 设置文件加载器（推荐，支持二进制和文本）
interpreter.set_file_loader(Box::new(|name| {
    let path = format!("game/{}", name);
    std::fs::read(&path).map_err(|e| e.into())
}));

// 加载外部脚本
interpreter.load_external_script("system/first.iet")?;
```

#### 执行控制

```rust
// 设置起始标签
interpreter.start("system/first.iet", "main")?;

// 便捷入口（自动检测 main/start/_start）
interpreter.boot("system/first.iet")?;

// 执行到下一个等待点
let result = interpreter.step()?;

// 持续执行
let result = interpreter.run()?;

// 处理执行结果
match result {
    ExecutionResult::Completed => println!("执行完成"),
    ExecutionResult::Wait(event) => println!("等待事件: {:?}", event),
    ExecutionResult::CallScript { file, label } => {
        println!("调用脚本: {} at {}", file, label)
    },
    ExecutionResult::JumpScript { file, label } => {
        println!("跳转脚本: {} at {}", file, label)
    },
}
```

#### 事件回调

```rust
interpreter.set_callback(|event| {
    match event {
        Event::Layer(LayerEvent::Create { id, file }) => {
            println!("创建图层: {} from {}", id, file);
        }
        Event::Trans { trans_type, time, .. } => {
            println!("转场: type={}, time={:?}", trans_type, time);
        }
        Event::BgmPlay { file, loop_play, .. } => {
            println!("播放 BGM: {}, loop={}", file, loop_play);
        }
        Event::ScenarioText { content, inline } => {
            println!("文本: {}", content);
        }
        Event::Wait { reason } => {
            println!("等待: {:?}", reason);
            return CallbackResult::Pause;  // 暂停执行
        }
        _ => {}
    }
    CallbackResult::Continue  // 继续执行
});
```

#### Lua 集成

```rust
// 设置自定义 Lua engine 回调
use asb_interpreter::{EngineCallbacks, EngineContext};

struct MyCallbacks;

impl EngineCallbacks for MyCallbacks {
    fn debug(&self, level: i32, data: &str, raw: bool) {
        println!("[DEBUG] {}", data);
    }
    
    fn get_script_status(&self) -> u8 {
        0  // 执行中
    }
    
    fn is_key_down(&self, key_id: u32) -> bool {
        false
    }
    
    // ... 实现其他方法
}

interpreter.set_engine_callbacks(Box::new(MyCallbacks));

// 获取 Lua 上下文
let lua = interpreter.lua();

// 获取 engine 上下文
let engine_ctx = interpreter.engine_context();
```

#### 变量管理

```rust
// 设置变量
interpreter.set_variable("score", Value::Int(100));
interpreter.set_variable("g.global_var", Value::String("value".into()));

// 获取变量
if let Some(Value::Int(score)) = interpreter.get_variable("score") {
    println!("Score: {}", score);
}

// 获取变量存储（用于存档）
let vars = interpreter.variables();
let saved = vars.save()?;

// 恢复变量（用于读档）
let vars = VariableStore::load(&saved)?;
interpreter.restore_variables(vars);
```

#### 标签注册

```rust
use asb_interpreter::{TagHandler, ExecutionContext, TagResult};

struct MyTag;

impl TagHandler for MyTag {
    fn execute(&self, ctx: &mut ExecutionContext) -> Result<TagResult> {
        let param = ctx.instruction.get("param").unwrap_or("");
        println!("MyTag: {}", param);
        Ok(TagResult::Continue)
    }
}

interpreter.register_tag("mytag", MyTag);
```

#### 配置访问

```rust
// 获取配置
let config = interpreter.config();
println!("Stage: {}x{}", config.stage_width, config.stage_height);

// 获取当前状态
if let Some(script) = interpreter.current_script() {
    println!("Current script: {}", script);
}
let line = interpreter.current_line();
println!("Current line: {}", line);

// 获取脚本
if let Some(script) = interpreter.get_script("system/first.iet") {
    println!("Instructions: {}", script.instructions.len());
}
```

---

### InterpreterConfig

解释器配置。

```rust
pub struct InterpreterConfig {
    /// 脚本编码（默认 UTF-8）
    pub encoding: &'static encoding_rs::Encoding,
    
    /// 舞台宽度
    pub stage_width: u32,
    /// 舞台高度
    pub stage_height: u32,
    /// 帧率
    pub fps: u32,
    
    /// 无边框窗口
    pub frameless: bool,
    /// 可调整大小
    pub resizable: bool,
    /// 固定宽高比
    pub fixed_aspect_ratio: bool,
    /// 裁剪溢出
    pub sidecut: bool,
    /// 侧边图片
    pub side_picture: Option<String>,
    
    /// 节能模式
    pub power_saving: bool,
    /// 禁用存档
    pub no_save: bool,
    
    /// 存档路径
    pub savepath: Option<String>,
    /// 数据路径
    pub datapath: Option<String>,
    /// 游戏标题
    pub title: Option<String>,
    
    /// 进程 ID（防止多重启动）
    pub process_id: Option<String>,
    
    /// 自定义环境变量
    pub env: HashMap<String, String>,
}
```

---

### Event

事件枚举。

```rust
pub enum Event {
    // 图层事件
    Layer(LayerEvent),
    
    // 转场事件
    Trans {
        trans_type: i32,
        time: Option<u64>,
        rule: Option<String>,
        vague: Option<i32>,
        input: i32,
    },
    Flip,
    UiTransition(TransitionEvent),
    
    // 音频事件
    BgmPlay {
        file: String,
        loop_play: bool,
        gain: Option<i32>,
        pan: Option<i32>,
        fade_time: Option<u64>,
    },
    BgmStop { fade_time: Option<u64> },
    BgmFade { gain: i32, time: u64 },
    BgmPan { pan: i32 },
    BgmCrossFade { /* ... */ },
    SePlay { /* ... */ },
    SeStop { /* ... */ },
    SeFade { /* ... */ },
    SePan { /* ... */ },
    VoicePlay { /* ... */ },
    SoundFinishHandler { /* ... */ },
    SoundFinishHandlerDel { id: String },
    
    // 剧情事件
    ScenarioText {
        content: String,
        inline: bool,
    },
    LineBreak,
    PageBreak { backlog: Option<i32> },
    FontSettings(HashMap<String, String>),
    FontClose,
    FontDefault(HashMap<String, String>),
    FontInit,
    RubyStart { text: String },
    RubyEnd,
    LinkStart { /* ... */ },
    LinkEnd,
    LinkDisable,
    LinkEnable,
    GlyphConfig(HashMap<String, String>),
    MessageLayerSwitch { id: Option<String>, layered: i32 },
    MessageLayerPop,
    TextAnimation(HashMap<String, String>),
    SceneIn,
    SceneOut,
    
    // 等待事件
    Wait { reason: WaitReason },
    
    // 系统事件
    Exec { command: String, mode: Option<i32> },
    SaveGame { file: String },
    LoadGame { file: String, trans_type: Option<i32> },
    DebugConfig { mode: Option<i32>, level: Option<i32> },
    DebugPrint { level: i32, data: String },
    DebugReload,
    Caption { data: String },
    MouseConfig { /* ... */ },
    KeyConfig(HashMap<String, String>),
    FileOperation { /* ... */ },
    HttpGet { url: String },
    HttpPost { url: String, params: HashMap<String, String> },
    OpenBrowser { url: String },
    AutoSaveConfig { allow: bool },
    AvoidConfig { allow: bool },
    Vibrate { time: u64 },
    StatusBar { visible: bool },
    Purchase { item: String },
    CallNative { function: String, params: HashMap<String, String> },
    Dialog { title: String, message: String },
    YesNo { file: String, se: Option<String> },
    Exit,
    GoTitle,
    
    // 事件处理器
    SetEventHandler {
        event_name: String,
        file: Option<String>,
        label: Option<String>,
        call: bool,
        handler: Option<String>,
    },
    DelEventHandler { event_name: String },
    
    // 状态事件
    LoadingState { active: bool },
    SavingState { active: bool },
    LoadMask { action: LoadMaskAction, time: u64 },
    AllDelete { time: u64 },
    SystemUi { action: SystemUiAction },
    SaveOperation { action: SaveAction },
    Repeatedly,
    AutoSkipDisable,
    
    // 视频/动画
    VideoPlay { /* ... */ },
    VideoFinishHandler { /* ... */ },
    VideoFinishHandlerDel,
    Anime { /* ... */ },
    
    // 图层高级
    LayerTween { /* ... */ },
    LayerTweenDelete { id: String },
    TweenSetStart,
    TweenSetEnd,
    LayerEventHandler { /* ... */ },
    LayerRename { id: String, to: String },
    LayerEdit { /* ... */ },
    LayerDrag { id: String },
    
    // 截图
    TakeScreenshot,
    SaveScreenshot { file: String },
    
    // 右键菜单
    RightClickConfig { allow: bool, file: Option<String> },
    
    // 宏
    MacroDel { file: String },
    
    // 自定义事件
    Custom {
        tag: String,
        params: HashMap<String, String>,
    },
}
```

---

### CallbackResult

回调结果。

```rust
pub enum CallbackResult {
    /// 继续执行
    Continue,
    /// 暂停执行
    Pause,
    /// 中止执行
    Abort,
}
```

---

### ExecutionResult

执行结果。

```rust
pub enum ExecutionResult {
    /// 执行完成
    Completed,
    /// 等待事件
    Wait(Event),
    /// 调用脚本
    CallScript { file: String, label: String },
    /// 跳转脚本
    JumpScript { file: String, label: String },
}
```

---

### Value

变量值。

```rust
pub enum Value {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Null,
}
```

---

### VariableStore

变量存储。

```rust
pub struct VariableStore {
    // 私有字段
}

impl VariableStore {
    pub fn new() -> Self;
    pub fn get(&self, name: &str) -> Option<&Value>;
    pub fn set(&mut self, name: &str, value: Value);
    pub fn remove(&mut self, name: &str) -> Option<Value>;
    pub fn contains(&self, name: &str) -> bool;
    pub fn clear_temp(&mut self);
    pub fn clear_all(&mut self);
    pub fn reset(&mut self);
    pub fn save(&self) -> Result<Vec<u8>>;
    pub fn load(data: &[u8]) -> Result<Self>;
}
```

---

### EngineCallbacks

Lua engine 回调 trait。

```rust
pub trait EngineCallbacks: Send + Sync {
    fn debug(&self, level: i32, data: &str, raw: bool);
    fn enqueue_tag(&self, tag: String, params: HashMap<String, String>);
    fn set_event_handler(&self, handlers: HashMap<String, String>);
    fn get_script_status(&self) -> u8;
    fn is_key_down(&self, key_id: u32) -> bool;
    fn is_key_down_edge(&self, key_id: u32) -> bool;
    fn is_key_up_edge(&self, key_id: u32) -> bool;
    fn is_decide(&self) -> bool;
    fn get_mouse_point(&self) -> (i32, i32);
    fn get_touch_count(&self) -> u32;
    fn get_touch_point(&self, index: u32) -> (i32, i32);
    fn is_file_exists(&self, path: &str) -> bool;
    fn file_operation(&self, command: &str, params: HashMap<String, String>);
    fn include(&self, path: &str);
    fn override_key(&self, from: u32, to: u32);
    fn set_flick_sensitivity(&self, sensitivity: f64);
    fn get_script_block(&self) -> HashMap<String, String>;
    fn get_script_stack(&self) -> Vec<HashMap<String, String>>;
    fn get_script_wait_reason(&self) -> u8;
}
```

---

### Lua Engine API

在 Lua 脚本中可用的 engine 对象方法：

```lua
-- 调试输出
e:debug{level=0, data="message", raw=false}
e:debug("message")

-- 执行标签
e:tag{"tagname", param1="value1", param2="value2"}
e:enqueueTag{"tagname", param1="value1"}

-- 事件处理器
e:setEventHandler{onEnterFrame="func", onClickWaitIn="func"}

-- 脚本状态
e:getScriptStatus()  -- 返回 0-14
e:getScriptBlock()
e:getScriptStack()
e:getScriptWaitReason()

-- 输入
e:isPush(key_id)
e:isDown(key_id)
e:isDownEdge(key_id)
e:isUpEdge(key_id)
e:isDecide()
e:getMousePoint()
e:getTouchCount()
e:getTouchPoint(index)

-- 时间
e:now()  -- 毫秒
e:random()  -- 0.0-1.0

-- 文件
e:include("path")
e:isFileExists("path")
e:file{command="copy", src="a", dst="b"}

-- 其他
e:overrideKey(from, to)
e:setFlickSensitivity(sensitivity)
```

---

## 完整示例

### 基础使用

```rust
use asb_interpreter::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 创建解释器
    let config = InterpreterConfig {
        stage_width: 1280,
        stage_height: 720,
        fps: 60,
        ..Default::default()
    };
    let mut interpreter = Interpreter::new(config);

    // 2. 设置文件加载器
    interpreter.set_file_loader(Box::new(|name| {
        let path = format!("game/{}", name);
        std::fs::read(&path).map_err(|e| e.into())
    }));

    // 3. 设置事件回调
    interpreter.set_callback(|event| {
        match event {
            Event::ScenarioText { content, .. } => {
                println!("【文本】{}", content);
            }
            Event::Layer(LayerEvent::Create { id, file }) => {
                println!("【图层】创建 {} from {}", id, file);
            }
            Event::BgmPlay { file, loop_play, .. } => {
                println!("【BGM】播放 {} (loop={})", file, loop_play);
            }
            Event::Trans { trans_type, time, .. } => {
                println!("【转场】type={}, time={:?}", trans_type, time);
            }
            Event::Wait { reason } => {
                println!("【等待】{:?}", reason);
                return CallbackResult::Pause;
            }
            _ => {}
        }
        CallbackResult::Continue
    });

    // 4. 启动解释器
    interpreter.boot("system/first.iet")?;

    // 5. 执行循环
    loop {
        match interpreter.run()? {
            ExecutionResult::Completed => break,
            ExecutionResult::Wait(event) => {
                // 等待用户输入
                println!("等待事件: {:?}", event);
                // ... 处理用户输入
                // interpreter.resume();  // 恢复执行
            }
            _ => {}
        }
    }

    Ok(())
}
```

### 存档/读档

```rust
// 存档
fn save_game(interpreter: &Interpreter) -> Result<Vec<u8>> {
    let vars = interpreter.variables();
    vars.save()
}

// 读档
fn load_game(interpreter: &mut Interpreter, data: &[u8]) -> Result<()> {
    let vars = VariableStore::load(data)?;
    interpreter.restore_variables(vars);
    Ok(())
}
```

### 自定义标签

```rust
use asb_interpreter::*;

struct EffectTag;

impl TagHandler for EffectTag {
    fn execute(&self, ctx: &mut ExecutionContext) -> Result<TagResult> {
        let name = ctx.instruction.get("name").unwrap_or("");
        let duration = ctx.instruction.get("duration")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1000);
        
        println!("播放特效: {} ({}ms)", name, duration);
        
        Ok(TagResult::Emit(Event::Custom {
            tag: "effect".to_string(),
            params: {
                let mut m = HashMap::new();
                m.insert("name".to_string(), name.to_string());
                m.insert("duration".to_string(), duration.to_string());
                m
            },
        }))
    }
}

interpreter.register_tag("effect", EffectTag);
```

---

## 错误处理

```rust
pub enum Error {
    ParseError { line: usize, message: String },
    RuntimeError { line: usize, message: String },
    ExpressionError(String),
    UndefinedVariable(String),
    LabelNotFound(String),
    ScriptNotFound(String),
    LuaError(mlua::Error),
    IoError(std::io::Error),
    SerializeError(String),
    DecodeError(String),
    Aborted,
}
```

---

## 性能考虑

- **迭代式执行**：使用 `loop` 而非递归，避免栈溢出
- **事件驱动**：通过回调处理事件，不阻塞执行
- **智能加载**：自动检测文件格式，支持文本和二进制
- **Lua 集成**：使用 mlua，支持 Lua 5.4

---

## 测试覆盖

- ✅ 25 个单元测试
- ✅ 8 个集成测试
- ✅ 6 个脚本测试
- ✅ 1 个文档测试

**总计：40 个测试全部通过**

---

## 许可证

MIT
