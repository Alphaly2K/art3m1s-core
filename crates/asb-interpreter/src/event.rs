//! 事件系统
//!
//! 定义解释器在执行过程中产生的各种事件，
//! 以及用户通过回调处理这些事件的机制。

use std::collections::HashMap;

/// 解释器事件
#[derive(Debug, Clone)]
pub enum Event {
    /// UI 转场效果
    UiTransition(TransitionEvent),

    /// 图层操作
    Layer(LayerEvent),

    /// 加载状态变化
    LoadingState {
        /// 是否激活
        active: bool,
    },

    /// 存档状态变化
    SavingState {
        /// 是否激活
        active: bool,
    },

    /// 音效播放
    PlaySound {
        /// 音效名称
        name: String,
        /// 是否等待播放完成
        wait: bool,
    },

    /// 停止所有音效
    StopAllSounds {
        /// 淡出时间（毫秒）
        duration: u64,
    },

    /// 对话框显示
    ShowDialog {
        /// 标题
        title: String,
        /// 消息内容
        message: String,
        /// 接收确定/取消结果（1/0）的变量名
        varname: Option<String>,
        /// 接收文本输入的变量名
        textfield: Option<String>,
        /// 文本输入最大字符数
        textfield_size: Option<usize>,
    },

    /// 是/否选择
    YesNo {
        /// 配置文件
        file: String,
        /// 音效
        se: Option<String>,
    },

    /// 脚本调用（call）
    ScriptCall {
        /// 脚本文件
        file: String,
        /// 标签名
        label: String,
    },

    /// 退出请求
    Exit,

    /// 返回标题
    GoTitle,

    /// 通用等待
    Wait {
        /// 等待原因
        reason: WaitReason,
    },

    /// 剧情文本
    Text {
        /// 文本内容
        content: String,
    },

    /// 加载遮罩
    LoadMask {
        /// 操作类型
        action: LoadMaskAction,
        /// 时间（毫秒）
        time: u64,
    },

    /// 全部删除
    AllDelete {
        /// 时间（毫秒）
        time: u64,
    },

    /// 系统 UI 显示/隐藏
    SystemUi {
        /// 操作类型
        action: SystemUiAction,
    },

    /// 存档操作
    SaveOperation {
        /// 操作类型
        action: SaveAction,
    },

    /// 重复执行标记
    Repeatedly,

    /// 自动跳过禁用
    AutoSkipDisable,

    // ── 剧情文本事件 ──────────────────────────────────────
    /// 剧情文本
    ScenarioText { content: String, inline: bool },
    /// 换行 [rt]
    LineBreak {
        /// 1 时若最后一行为空行则不换行（防止意外空行）
        omitblankline: bool,
    },
    /// 分页 [rp]
    PageBreak { backlog: Option<i32> },
    /// 字体设置 [font]
    FontSettings(HashMap<String, String>),
    /// 回退字体 [font_close]
    FontClose,
    /// 默认字体设置 [fontdefault]
    FontDefault(HashMap<String, String>),
    /// 初始化字体 [fontinit]
    FontInit,
    /// 开始注音 [ruby]
    RubyStart { text: String },
    /// 结束注音 [/ruby]
    RubyEnd,
    /// 开始链接 [link]
    LinkStart {
        file: Option<String>,
        label: Option<String>,
        link_type: i32,
        /// type=1 时强调显示的文字颜色（RRGGBB，缺省 0x000000）
        color: Option<String>,
        /// type=1 时强调显示的文字阴影颜色（RRGGBB，缺省 0x000000）
        shadowcolor: Option<String>,
        /// type=1 时强调显示的文字轮廓颜色（RRGGBB，缺省 0x000000）
        outlinecolor: Option<String>,
    },
    /// 结束链接 [/link]
    LinkEnd,
    /// 禁用链接
    LinkDisable,
    /// 启用链接
    LinkEnable,
    /// 点击等待图标 [glyph]
    GlyphConfig(HashMap<String, String>),
    /// 切换消息层 [chgmsg]
    MessageLayerSwitch {
        /// 目标消息层 ID。缺省时由解释器生成随机 ID（文档：缺省则设置为随机值）
        id: Option<String>,
        /// 是否把前一设置压入消息层堆栈（缺省 1；0 用于防止存档膨胀）
        stack: bool,
        /// 0=还原为常规消息层 / 1=视为图像层处理 / None（缺省）=独立于图像层之上
        layered: Option<i32>,
    },
    /// 回退消息层 [chgmsg_close]
    MessageLayerPop,
    /// 文本动画 [scetween]
    TextAnimation(HashMap<String, String>),
    /// 场景进入 [scein]
    SceneIn,
    /// 场景退出 [sceout]
    SceneOut,
    /// 自动模式配置 [automode]
    AutoModeConfig {
        allow: bool,
        /// 自动模式下自动显示并同步 visible 的图层 ID（缺省禁用）
        layer: Option<String>,
        /// 左键单击是否停止自动模式（默认 1；None=保留之前设置）
        stopbyclick: Option<bool>,
        /// stop 标签是否停止自动模式（默认 1；None=保留之前设置）
        stopbystop: Option<bool>,
        /// 等待播放结束的 SE ID 数组（None=保留之前设置）
        syncse: Option<Vec<String>>,
    },
    /// 跳过配置 [skip]（None=继承之前的设置）
    SkipConfig {
        allow: Option<bool>,
        skip_unread: Option<bool>,
    },
    /// 历史配置 [backlog]
    BacklogConfig {
        allow: bool,
        /// 用于历史文本的消息层 ID（默认 backlog）；None=继承先前设置
        messagelayer: Option<String>,
        /// 是否把字体信息一并写入历史文本（默认 1=写入）；None=继承先前设置
        includefont: Option<bool>,
        /// 进入历史文本时临时隐藏的图层 ID 列表；None=继承先前设置
        hide: Option<Vec<String>>,
        /// 进入历史文本时自动显示（与自动模式同步）的图层 ID；None=禁用自动显示
        layer: Option<String>,
        /// 1 时清除当前历史文本中已存储的剧情（缺省/0 不清除）
        clear: bool,
    },
    /// 隐藏配置 [hide]
    HideConfig {
        allow: bool,
        /// 隐藏时同时临时隐藏的图层 ID 列表；None（缺省）=继承之前的设置
        window: Option<Vec<String>>,
    },
    /// 已读判定 [alreadyread]（mode=0 不判定 / 1 判定，文档默认 1）
    AlreadyReadConfig { mode: i32 },
    /// 历史写入 [writebacklog]（mode=缺省/0 不存入，1 存入）
    WriteBacklogConfig { mode: bool },
    /// 缩进设置 [indent]
    IndentConfig {
        /// 每两个字符一组，交替列出缩进开始/结束字符（如 "「」『』"）
        pair: String,
        /// 行首起忽略此数目字符后出现的缩进开始字符（None=任意位置均识别）
        range: Option<usize>,
        /// true 时已处于缩进状态也重复嵌套缩进（缺省 false）
        nest: bool,
    },
    /// 禁则处理 [prohibit]
    ProhibitConfig {
        /// 行首禁则字符串（连续字符，无分隔符）
        head: String,
        /// 行尾禁则字符串（连续字符，无分隔符）
        foot: String,
    },
    /// 单词部分字符 [wordparts]
    WordpartsConfig {
        /// 视为单词组成部分的字符集合（连续字符，无分隔符）
        parts: String,
    },

    // ── 音频事件 ──────────────────────────────────────
    /// 播放 BGM [splay]
    BgmPlay {
        file: String,
        loop_play: bool,
        gain: Option<i32>,
        pan: Option<i32>,
        fade_time: Option<u64>,
        /// 缓冲区大小（毫秒），-1 表示内存播放（仅 Windows/WASM），None=默认缓冲
        buffer: Option<i32>,
    },
    /// 停止 BGM [sstop]
    BgmStop { fade_time: Option<u64> },
    /// BGM 音量渐变 [sfade]
    BgmFade { gain: i32, time: u64 },
    /// BGM 声像 [span]
    BgmPan {
        pan: i32,
        /// 渐变时间（毫秒），None=立即切换
        time: Option<u64>,
    },
    /// BGM 交叉淡入 [sxfade]
    BgmCrossFade {
        file: String,
        loop_play: bool,
        gain: Option<i32>,
        pan: Option<i32>,
        time: u64,
    },
    /// 播放 SE [seplay]
    SePlay {
        id: String,
        file: String,
        loop_play: bool,
        gain: Option<i32>,
        pan: Option<i32>,
        fade_time: Option<u64>,
        skippable: bool,
    },
    /// 停止 SE [sestop]
    SeStop { id: String, fade_time: Option<u64> },
    /// SE 音量渐变 [sefade]
    SeFade { id: String, gain: i32, time: u64 },
    /// SE 声像 [sepan]
    SePan {
        id: String,
        pan: i32,
        /// 渐变时间（毫秒），None=立即切换
        time: Option<u64>,
    },
    /// 语音播放 [voice]（参数与 seplay 完全一致）
    VoicePlay {
        /// 语音轨 ID（缺省时由核心自动编号）
        id: Option<String>,
        file: String,
        loop_play: bool,
        gain: Option<i32>,
        pan: Option<i32>,
        fade_time: Option<u64>,
        /// 1 时跳过模式下不播放
        skippable: bool,
    },
    /// 音效完成事件处理器 [setonsoundfinish]
    SoundFinishHandler {
        id: String,
        file: Option<String>,
        label: Option<String>,
        call: bool,
        handler: Option<String>,
    },
    /// 解除音效完成事件处理器 [delonsoundfinish]
    SoundFinishHandlerDel { id: String },

    // ── 系统操作事件 ──────────────────────────────────────
    /// 执行用户操作 [exec]
    Exec { command: String, mode: Option<i32> },
    /// 存档 [save]
    SaveGame { file: String },
    /// 读档 [load]
    LoadGame {
        file: String,
        trans_type: Option<i32>,
    },
    /// 调试设置 [debug]
    DebugConfig {
        mode: Option<i32>,
        level: Option<i32>,
    },
    /// 调试输出 [debugprint]
    DebugPrint { level: i32, data: String },
    /// 调试重载 [debugreload]
    DebugReload,
    /// 重启引擎 [reset]：宿主应重置合成器/音频/控制状态并重新走 boot 管线
    Reset,
    /// 窗口标题 [caption]
    Caption { data: String },
    /// 鼠标设置 [mouse]
    MouseConfig {
        left: Option<i32>,
        top: Option<i32>,
        hide: Option<i32>,
        autohide: Option<u64>,
    },
    /// 按键配置 [keyconfig]
    KeyConfig(HashMap<String, String>),
    /// 文件操作 [file]
    FileOperation {
        command: String,
        src: Option<String>,
        dst: Option<String>,
        target: Option<String>,
        /// wasm_sync：要同步的文件列表 URL（其余 command 忽略）
        url: Option<String>,
        /// wasm_sync：要同步文件的基准 URL（其余 command 忽略）
        baseurl: Option<String>,
        /// wasm_sync：要同步的文件列表（其余 command 忽略）
        list: Option<Vec<String>>,
    },
    /// HTTP GET [httpget]
    HttpGet {
        url: String,
        /// 额外请求头（header_keyN/header_valueN 成对，按 N 升序）
        headers: Vec<(String, String)>,
        /// 存储响应码的变量名
        varname_code: Option<String>,
        /// 存储响应体的变量名（指定 filename 时忽略）
        varname_data: Option<String>,
        /// 把结果存到文件而非变量
        filename: Option<String>,
    },
    /// HTTP POST [httppost]
    HttpPost {
        url: String,
        /// 额外请求头（header_keyN/header_valueN 成对，按 N 升序）
        headers: Vec<(String, String)>,
        /// POST 数据键值（keyN/valueN 成对，按 N 升序）
        data: Vec<(String, String)>,
        /// 以文件内容为值的 POST 数据（keyN/fileN 成对，值为文件路径）
        file_data: Vec<(String, String)>,
        /// 存储响应码的变量名
        varname_code: Option<String>,
        /// 存储响应体的变量名（指定 filename 时忽略）
        varname_data: Option<String>,
        /// 把结果存到文件而非变量
        filename: Option<String>,
    },
    /// 打开浏览器 [openbrowser]
    OpenBrowser { url: String },
    /// 自动存档配置 [autosave]
    ///
    /// allow：0=禁用；1=退出/切后台时自动保存；2=每次用户输入等待时自动保存
    AutoSaveConfig { allow: i32 },
    /// 紧急回避配置 [avoid]
    AvoidConfig {
        /// 紧急回避图像文件路径，None（缺省）=禁用紧急回避功能
        file: Option<String>,
        /// 回避期间窗口按钮行为：0=禁用 / 1=默认操作 / 2=退出回避并执行处理器
        windowbutton: i32,
    },
    /// 振动 [vibrate]
    Vibrate { time: u64 },
    /// 状态栏 [statusbar]
    StatusBar { visible: bool },
    /// 应用内购买 [purchase]（仅 iOS/Android）
    Purchase {
        /// false=仅获取商品信息不购买；缺省或 1=执行购买
        purchase: bool,
        /// 结果存储变量名
        varname: Option<String>,
        /// iOS：商品 ID
        productid: Option<String>,
        /// iOS：true=执行恢复流程（此时 purchase 被忽略）
        restore: bool,
        /// Android：Google Play 许可密钥
        key: Option<String>,
        /// Android：商品 ID
        sku: Option<String>,
        /// Android：true=执行消耗流程
        consume: bool,
    },
    /// 调用原生代码 [callnative]
    CallNative {
        /// 存储原生代码返回字符串的变量名
        result: Option<String>,
        /// 模块（Windows: DLL 路径；iOS: 类名；Android: JNI 完整类名）
        module: Option<String>,
        /// 函数名/选择器名/方法名（WASM 时为直接 eval 的 JS 代码）
        method: String,
        /// 传给函数的字符串
        param: Option<String>,
    },

    // ── 事件处理器事件 ──────────────────────────────────────
    /// 设置事件处理器 [seton*]
    SetEventHandler {
        event_name: String,
        file: Option<String>,
        label: Option<String>,
        call: bool,
        handler: Option<String>,
        /// 标签里除已知字段外的其它参数（key、adv、ui、btn 等），
        /// 由宿主在事件触发时作为 param 传给 Lua 回调。
        extra_params: std::collections::HashMap<String, String>,
    },
    /// 解除事件处理器 [delon*]
    DelEventHandler {
        event_name: String,
        /// 指定 key 时只解除该键的处理器；None 时解除整个事件类型的所有处理器。
        key: Option<String>,
    },

    // ── 图层缓动 ──────────────────────────────────────
    /// 图层属性缓动 [lytween]
    LayerTween {
        id: String,
        param: String,
        from: Option<String>,
        to: Option<String>,
        ease: Option<String>,
        time: Option<u64>,
        delay: Option<u64>,
        loop_count: Option<i32>,
        yoyo: Option<i32>,
        loop_delay: Option<u64>,
        sync: bool,
        delete: bool,
        handler_file: Option<String>,
        handler_label: Option<String>,
        handler_handler: Option<String>,
    },
    /// 强制完成图层缓动 [lytweendel]
    LayerTweenDelete { id: String },
    /// 缓动序列开始 [tweenset]
    TweenSetStart,
    /// 缓动序列结束 [/tweenset]
    TweenSetEnd,

    // ── 图层事件/操作 ──────────────────────────────────────
    /// 图层事件处理器 [lyevent]
    LayerEventHandler {
        id: String,
        event_type: String,
        mode: String,
        file: Option<String>,
        label: Option<String>,
        call: bool,
        handler: Option<String>,
        penetration: bool,
        /// 标签里除已知字段外的其它参数（name、key、se、function 等），
        /// 触发事件时由宿主原样塞进 handler 标签的参数表。
        extra_params: std::collections::HashMap<String, String>,
    },
    /// 图层重命名 [lyrename]
    LayerRename { id: String, to: String },
    /// 图层图像编辑 [lyedit]
    LayerEdit {
        id: String,
        mode: String,
        color: Option<String>,
        file: Option<String>,
        left: Option<i32>,
        top: Option<i32>,
    },
    /// 图层拖动 [lydrag]
    LayerDrag { id: String },

    // ── 动画/视频 ──────────────────────────────────────
    /// 帧动画 [anime]
    Anime {
        id: String,
        mode: String,
        file: Option<String>,
        mask: Option<String>,
        time: Option<u64>,
        loop_count: Option<i32>,
        props: HashMap<String, String>,
    },
    /// 视频播放 [video]
    VideoPlay {
        id: Option<String>,
        file: String,
        /// 0=禁止跳过 / 1=单击跳过（缺省）/ 2=仅右键菜单方式跳过
        skip: i32,
        loop_play: bool,
        /// Ogg Theora 视频图层的跳帧延迟阈值（毫秒）；None（缺省/-1）=不跳帧
        delay_margin_ms: Option<i32>,
        /// 仅 Windows 全屏：0=VMR-7 / 1=VMR-9 / 2=EVR（其它平台忽略）
        mode: Option<i32>,
    },
    /// 视频播放完成事件处理器 [setonvideofinish]
    VideoFinishHandler {
        /// 目标图层 ID
        id: Option<String>,
        file: Option<String>,
        label: Option<String>,
        call: bool,
        handler: Option<String>,
    },
    /// 解除视频播放完成事件处理器 [delonvideofinish]
    VideoFinishHandlerDel {
        /// 待取消的图层 ID
        id: Option<String>,
    },

    // ── 转场/截图 ──────────────────────────────────────
    /// 图层树转换 [trans]
    Trans {
        trans_type: i32,
        time: Option<u64>,
        rule: Option<String>,
        vague: Option<i32>,
        input: i32,
    },
    /// 立即反映图层变更 [flip]
    Flip,
    /// 注册图层 HLSL shader [lyshader]
    ShaderLoad { id: String, file: String },
    /// 截图 [takess]
    TakeScreenshot,
    /// 保存截图 [savess]
    SaveScreenshot {
        file: String,
        width: Option<u32>,
        height: Option<u32>,
    },

    // ── 脚本/宏 ──────────────────────────────────────
    /// 右键单击脚本 [rclick]
    RightClickConfig { allow: bool, file: Option<String> },
    /// 解除宏定义文件 [macrodel]
    MacroDel { file: String },

    /// 自定义事件（未识别的标签）
    Custom {
        /// 标签名
        tag: String,
        /// 参数
        params: HashMap<String, String>,
    },
}

/// UI 转场事件
#[derive(Debug, Clone)]
pub struct TransitionEvent {
    /// 转场时间（毫秒）
    pub time: u64,
    /// 转场类型
    pub fade: Option<String>,
}

/// 图层事件
#[derive(Debug, Clone)]
pub enum LayerEvent {
    /// 创建图层
    Create { id: String, file: String },
    /// 创建图层（变体）
    Create2 {
        id: String,
        file: String,
        alpha: Option<u8>,
    },
    /// 删除图层
    Delete { id: String },
    /// 设置图层属性
    SetProperty {
        id: String,
        property: String,
        value: String,
    },
    /// 批量设置图层属性（用于图层集）
    SetProperties {
        id: String,
        properties: HashMap<String, String>,
    },
}

/// 图层属性
#[derive(Debug, Clone, Default)]
pub struct LayerProperties {
    /// 左边位置
    pub left: Option<i32>,
    /// 顶部位置
    pub top: Option<i32>,
    /// 宽度
    pub width: Option<i32>,
    /// 高度
    pub height: Option<i32>,
    /// 透明度 (0-255)
    pub alpha: Option<u8>,
    /// 是否可见
    pub visible: Option<bool>,
    /// 缩放 X
    pub scale_x: Option<f32>,
    /// 缩放 Y
    pub scale_y: Option<f32>,
    /// 旋转角度
    pub rotation: Option<f32>,
    /// 其他自定义属性
    pub custom: HashMap<String, String>,
}

/// 等待原因
#[derive(Debug, Clone)]
pub enum WaitReason {
    /// 通用等待 [wt]
    Generic,
    /// 通用等待（变体）[wt0]
    Generic0,
    /// 停止 [stop]
    Stop {
        /// 停止原因
        reason: Option<String>,
    },
    /// 按键等待 [exkey]
    KeyWait {
        /// 按钮列表
        buttons: Vec<String>,
    },
    /// 时间等待 [wait time="xxx" input="x"]
    Timed {
        /// 毫秒数
        milliseconds: u64,
        /// 输入策略：0=不接受输入，1=输入解除等待，2=跳过中不停止
        input: i32,
    },
    /// 等待 SE 播放结束 [wait se="ID"]
    Se {
        /// SE 的 ID
        id: String,
        /// 与 time 参数并用时的等待毫秒数——从该 SE **开始播放的时刻**起算
        /// （文档 wait.md）；`None` 表示等到 SE 播放结束
        time: Option<u64>,
    },
    /// 等待视频图层播放结束 [wait video="层ID"]
    VideoLayer {
        /// 视频图层 ID
        id: String,
    },
    /// 等待场景文本 Tween 完成 [wait scenario="1|2"]
    ScenarioTween {
        /// 1=等待场景文本出现的 Tween 完成，2=等待场景文本隐藏的 Tween 完成
        mode: i32,
    },
}

/// 加载遮罩操作
#[derive(Debug, Clone)]
pub enum LoadMaskAction {
    /// 显示
    Show,
    /// 删除
    Delete,
}

/// 系统 UI 操作
#[derive(Debug, Clone)]
pub enum SystemUiAction {
    /// 显示
    Show,
    /// 隐藏
    Hide {
        /// 跳过模式
        skip: Option<String>,
    },
}

/// 存档操作
#[derive(Debug, Clone)]
pub enum SaveAction {
    /// 系统存档
    SystemSave,
    /// 系统读档
    SystemLoad,
}

/// 回调结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackResult {
    /// 继续执行
    Continue,
    /// 暂停执行（等待外部事件）
    Pause,
    /// 中止执行
    Abort,
}

/// 事件回调函数类型
pub type EventCallback = Box<dyn FnMut(Event) -> CallbackResult + Send + Sync>;

/// 脚本加载器函数类型（返回文本）
pub type ScriptLoader = Box<dyn Fn(&str) -> crate::error::Result<String> + Send + Sync>;

/// 脚本文件加载器函数类型（返回原始字节，支持文本和二进制）
pub type ScriptFileLoader = Box<dyn Fn(&str) -> crate::error::Result<Vec<u8>> + Send + Sync>;

/// 默认回调，继续执行所有事件
pub fn default_callback(_event: Event) -> CallbackResult {
    CallbackResult::Continue
}
