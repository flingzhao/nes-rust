# 三角波声道实现说明

本文件整理 2026 年 10 月 7 日这一轮三角波任务，不增加新任务，也不改变已约定的接口。它是实现要求，不表示源码已经实现或测试已经通过。

## 一、本轮目标与调用关系

游戏通过 CPU 写入三角波寄存器后，模拟器应能够：

1. 设置波形变化的速度。
2. 设置声音持续运行的时间。
3. 随 CPU 时钟产生三角波电平。
4. 持续时间用完后，停止波形变化，保持最后的电平。

本轮只生成 0～15 的原始输出电平，不涉及扬声器播放。

```text
游戏写寄存器
    ↓
Apu::write_register()
    ↓
TriangleChannel::write_register() / set_enabled()
    ↓
保存波形速度、持续时间等设置

每个 CPU 周期
    ↓
Apu::clock_cpu_cycle()
    ├─ TriangleChannel::clock_cpu_cycle()：推进定时器及波形步骤
    └─ FrameCounter 产生事件
         ├─ quarter → TriangleChannel::clock_quarter_frame()
         └─ half    → TriangleChannel::clock_half_frame()

需要取得当前电平
    ↓
Apu::triangle_output()
    ↓
TriangleChannel::output()
```

quarter-frame 和 half-frame 是 FrameCounter 发出的事件，不是替代普通 CPU 周期的运行模式。quarter 和 half 可以同时发生，不能使用 `if ... else if ...` 让它们只执行一个。

## 二、完整的数据定义

### 1. 波形表 TRIANGLE_SEQUENCE

波形表决定每一个波形步骤对应的输出电平：

```rust
const TRIANGLE_SEQUENCE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8,
     7,  6,  5,  4,  3,  2, 1, 0,
     0,  1,  2,  3,  4,  5, 6, 7,
     8,  9, 10, 11, 12, 13, 14, 15,
];
```

当前波形步骤 `sequence_step` 是数组下标，不是输出电平。例如：

```text
sequence_step = 5 → output() = TRIANGLE_SEQUENCE[5] = 10
sequence_step = 6 → output() = TRIANGLE_SEQUENCE[6] = 9
```

### 2. 长度表 LENGTH_TABLE

长度表提供长度计数器 `length_counter` 的初始计数，不决定输出电平。它是 NES APU 规定的固定映射，不从游戏 ROM 读取。三角波与脉冲声道共用同一张表即可。

```rust
const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6,
    160, 8, 60, 10, 14, 12, 26, 14,
    12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];
```

例如，游戏向 `$400B` 写入 `0x18`，长度表索引为 `0x18 >> 3 = 3`，对应 `LENGTH_TABLE[3] = 2`。声道已启用时，长度计数器 `length_counter` 被装成 2；没有暂停长度递减时，两个 half-frame 事件使它经历 `2 → 1 → 0`。这里的 2 不是输出高度。

若长度表目前位于脉冲声道模块，可以将其移动到父模块 `apu`，由两个声道共享，避免复制两份。

### 3. 三角波状态 TriangleChannel

```rust
pub(super) struct TriangleChannel {
    enabled: bool,
    control: u8,
    timer_period: u16,
    timer: u16,
    sequence_step: u8,
    length_counter: u8,
    linear_counter: u8,
    linear_reload: bool,
}
```

| 中文含义与变量 | 保存什么 |
|---|---|
| 长度装载开关 `enabled` | 是否允许通过寄存器写入装载长度计数器 |
| 控制寄存器 `control` | `$4008` 写入的原始值 |
| 定时器重装值 `timer_period` | 每轮定时器从多少开始倒计数，控制波形速度 |
| 当前定时器计数 `timer` | 当前这一轮数到哪里 |
| 当前波形步骤 `sequence_step` | 波形表的下标，范围 0～31 |
| 长度计数器 `length_counter` | 按 half-frame 更新的持续时间计数 |
| 线性计数器 `linear_counter` | 按 quarter-frame 更新的持续时间计数 |
| 线性重装请求 `linear_reload` | 下次 quarter-frame 是否重新装载线性计数器 |

控制寄存器 `control` 的 bit 7 是控制位，bit 0～6 是线性计数器重装值。可以在函数内临时计算，不必增加结构体字段：

```rust
let control_flag = self.control & 0x80 != 0;
let reload_value = self.control & 0x7F;
```

长度计数器 `length_counter` 和线性计数器 `linear_counter` 都控制持续时间，不控制波形高度。二者都大于零，当前波形步骤 `sequence_step` 才允许前进。任意一个归零，步骤停止变化；输出保持最后的电平，不会变成方波，也不一定变成零。

## 三、初始化和接收游戏设置：三个函数

供父模块 `Apu` 调用的方法可以使用 `pub(super)` 可见性。以下方法描述不是完整实现代码。

### 1. new() -> Self

**调用方：** `Apu::new()`。

**目的：** 创建确定的初始状态。

- 长度装载开关 `enabled` 和线性重装请求 `linear_reload` 为 `false`。
- 其余字段为 `0`。

两个持续时间计数器都为零，所以波形暂时不能前进。在本轮的软件约定中，`sequence_step=0`，因此 `output()` 返回波形表第一个值 15。这不是声明真实机器上电时一定输出 15，也不表示声音已经开始播放。

### 2. write_register(&mut self, register: u8, value: u8)

**调用方：** `Apu::write_register()`。

**输入：** `register` 是相对于 CPU 地址 `$4008` 的偏移，范围 0～3；`value` 是游戏写入的一个字节。这些地址属于 CPU 地址空间中的 APU 寄存器。

| register | CPU 地址 | 操作 |
|---:|---|---|
| 0 | `$4008` | 保存 `control=value` |
| 1 | `$4009` | 忽略 |
| 2 | `$400A` | 更新 `timer_period` 的低 8 位 |
| 3 | `$400B` | 更新周期高 3 位、按条件装载长度、设置线性重装请求 |

偏移 2 更新周期低位，保留高三位：

```rust
self.timer_period =
    (self.timer_period & 0x0700) | u16::from(value);
```

偏移 3 必须完成三件事：

1. 更新周期高三位，保留低八位：

   ```rust
   self.timer_period =
       (self.timer_period & 0x00FF)
       | (u16::from(value & 0x07) << 8);
   ```

2. 如果 `enabled==true`，装载长度：

   ```rust
   self.length_counter = LENGTH_TABLE[usize::from(value >> 3)];
   ```

3. 无论是否启用，都设置线性重装请求：

   ```rust
   self.linear_reload = true;
   ```

这些写入操作不顺便修改当前定时器计数 `timer`、当前波形步骤 `sequence_step` 或当前线性计数 `linear_counter`。新周期用于之后定时器到期时的重装。

写 `$4008` 不设置线性重装请求 `linear_reload`。写 `$400B` 设置重装请求，但不立即修改线性计数器 `linear_counter`；真正的线性重装在 quarter-frame 中执行。

### 3. set_enabled(&mut self, enabled: bool)

**调用方：** `Apu::write_register()` 处理 `$4015` 时，将 bit 2 转成布尔值传入。

**操作：**

1. 保存 `self.enabled=enabled`。
2. 如果关闭，将长度计数器 `length_counter` 清零。
3. 如果开启，不自动恢复长度计数器 `length_counter`。

```text
关闭前：enabled=true，length_counter=10
关闭后：enabled=false，length_counter=0
再开启：enabled=true，length_counter 仍是 0
```

游戏还需要写 `$400B` 才能重新装载长度。不要在此重置波形步骤 `sequence_step`、定时器计数 `timer` 或线性计数器 `linear_counter`。关闭后停止前进，是因为长度计数器已经为零。

## 四、随时间运行：三个函数

### 4. clock_cpu_cycle(&mut self)

**调用方：** `Apu::clock_cpu_cycle()`，每个 CPU 时钟周期调用一次，不是每条 CPU 指令调用一次。

**目的：** 决定当前波形步骤 `sequence_step` 什么时候可以前进。

```text
如果 timer > 0：
    timer 减一
否则：
    timer = timer_period
    如果 length_counter > 0 且 linear_counter > 0：
        sequence_step 前进一步，在 32 回绕
```

此处只读取两个持续时间计数器，不递减它们。

例子：执行前 `timer_period=126`、`timer=0`、`sequence_step=5`、`length_counter=10`、`linear_counter=3`。执行后 `timer=126`、`sequence_step=6`，输出从 `TRIANGLE_SEQUENCE[5]=10` 变成 `TRIANGLE_SEQUENCE[6]=9`。

如果相同状态下 `linear_counter=0`，执行后 `timer` 仍重装为 126，但 `sequence_step` 保持 5，输出保持 10。这就是“定时器继续运行，但波形步骤不再前进”的具体含义。

三角波定时器每个 CPU 周期更新，不需要脉冲声道的 `cpu_phase`。定时器重装值为 126 时，每隔 127 个 CPU 周期获得一次前进机会。

### 5. clock_quarter_frame(&mut self)

**调用方：** `Apu` 收到 FrameCounter 的 quarter 事件时。

**目的：** 更新线性计数器 `linear_counter`，决定这项持续时间限制还剩多少。

依次执行：

1. 如果 `linear_reload==true`，令 `linear_counter=control & 0x7F`。
2. 否则，如果 `linear_counter>0`，将 `linear_counter` 减一。
3. 最后，如果 `control` 的 bit 7 为零，清除 `linear_reload`。

重装与递减是二选一，不能在同一次 quarter-frame 先重装再减一。

例如 `control=0x03`、`linear_reload=true`、`linear_counter=0`，连续事件后：

```text
第一次 quarter：linear_counter=3，linear_reload=false
第二次 quarter：linear_counter=2
第三次 quarter：linear_counter=1
第四次 quarter：linear_counter=0
```

归零后，`clock_cpu_cycle()` 中的波形前进条件就不再成立。

如果 `control=0x83` 且 `linear_reload=true`，请求不会被清除，后续每次 quarter 都重新装成 3。仅设置 `control` 的 bit 7 不会创建重装请求；请求仍然来自写 `$400B`。

本函数不修改当前定时器计数 `timer`、当前波形步骤 `sequence_step` 或长度计数器 `length_counter`。

### 6. clock_half_frame(&mut self)

**调用方：** `Apu` 收到 FrameCounter 的 half 事件时。

**目的：** 更新长度计数器 `length_counter`。

```text
如果 control 的 bit 7 为零，并且 length_counter > 0：
    length_counter 减一
否则：
    length_counter 不变
```

使用的是三角波控制寄存器的 bit 7，不是脉冲声道用于暂停长度的 bit 5。

```text
control=0x03，length_counter=10
一次 half 后：length_counter=9

control=0x83，length_counter=10
一次 half 后：length_counter 仍是 10
```

本函数不修改线性计数器 `linear_counter`、当前定时器计数 `timer` 或当前波形步骤 `sequence_step`。

同一个控制位分别作用于两个函数：quarter 中决定是否清除 `linear_reload`，half 中决定是否递减 `length_counter`。

## 五、读取状态和电平：两个函数

### 7. is_active(&self) -> bool

**调用方：** `Apu::read_status()`，用于生成 `$4015` 的 bit 2。

只返回：

```rust
self.length_counter > 0
```

这里的 active 表示长度计数器尚未归零，不表示当前一定有变化中的声音。例如 `length_counter=10`、`linear_counter=0` 时，`is_active()` 返回 `true`，但波形不能前进。

这是 `$4015` 的硬件状态含义，不能把两个计数器一起判断。

### 8. output(&self) -> u8

**调用方：** `Apu::triangle_output()`，之后供混音使用。

只读取当前波形电平：

```rust
TRIANGLE_SEQUENCE[usize::from(self.sequence_step)]
```

不修改任何状态。不要因为 `length_counter==0` 或 `linear_counter==0` 就返回 0。计数器归零时，已经由 `clock_cpu_cycle()` 阻止 `sequence_step` 前进；本函数继续返回该步骤的电平。

同样，不要复制脉冲声道 `timer_period<8` 就静音的规则。

## 六、接入 Apu

保持已有脉冲声道和 FrameCounter 接口，补充以下连接：

| Apu 中的位置 | 新增处理 |
|---|---|
| `new()` | 创建 `triangle: TriangleChannel` |
| `write_register()` 的 `$4008..=$400B` | 调用 `triangle.write_register((addr - 0x4008) as u8, value)` |
| `write_register()` 的 `$4015` | 调用 `triangle.set_enabled(value & 0x04 != 0)` |
| `read_status()` | 将 `triangle.is_active()` 放到返回值的 bit 2，保留原有状态位和 IRQ 清除行为 |
| `clock_cpu_cycle()` | 每次调用 `triangle.clock_cpu_cycle()` |
| quarter 事件处理 | 调用 `triangle.clock_quarter_frame()` |
| half 事件处理 | 调用 `triangle.clock_half_frame()` |
| 新增 `pub fn triangle_output(&self) -> u8` | 返回 `triangle.output()` |

如果保留了 Apu 上供测试使用的 `clock_quarter_frame()` 和 `clock_half_frame()`，也要接入三角波。自动事件可以调用这两个统一入口，不要又在外面额外调用一次，导致重复递减。

本轮继续采用之前的调用约定：先更新各声道定时器，再处理本周期 FrameCounter 返回的事件。这是当前软件的周期级约定，不表示已经覆盖所有硬件边沿竞争细节。

保留 `pulse_outputs()` 的原接口；剩余 `$400C..=$4013` 不在本轮实现范围。

## 七、验收与提交范围

继续使用已提供的 13 项完整测试，整个测试模块追加到三角波模块末尾。字段和方法命名与本文一致。

测试文件在 Windows 中的位置：

```text
C:/Users/10433/Documents/Codex/2026-08-20/chatgpt-3/triangle_channel_tests.rs
```

在当前 WSL 中对应的位置：

```text
/mnt/c/Users/10433/Documents/Codex/2026-08-20/chatgpt-3/triangle_channel_tests.rs
```

核心验收：

- 波形按 32 步循环，速度由 `timer_period` 决定。
- quarter 只更新线性计数器及其重装请求。
- half 只按条件递减长度计数器。
- 任意计数器归零，都停止修改 `sequence_step`，但不强制输出零。
- `$4015` 的三角波状态只反映 `length_counter`。
- 新增三角波不破坏已有脉冲声道与 FrameCounter 测试。

运行：

```bash
cargo test --lib --tests
```

完成后提供三角波实现、Apu 接入部分和测试结果即可。本文档生成不包含源码审查或测试运行。

完成上述范围后的建议提交消息：

```text
feat(apu): add triangle channel and linear counter
```

## 参考依据

- [NESdev：三角波寄存器、定时器与线性计数器](https://www.nesdev.org/wiki/APU_Triangle)
- [NESdev：长度计数器与长度表](https://www.nesdev.org/wiki/APU_Length_Counter)
- [NESdev：APU 状态寄存器与声道行为](https://www.nesdev.org/wiki/NES_APU)
- [Mesen：TriangleChannel 寄存器处理参考](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/APU/TriangleChannel.h)
- [Mesen：定时器配置与当前计数分离的实现参考](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/APU/ApuTimer.h)
