# Lab4 实验报告

## 一、实验目标

为 teac 编译器的**后端（AArch64 汇编生成阶段）**补齐对 `f32` 浮点的支持，使浮点运算能够一路从 IR 下降到正确的 AArch64 汇编并通过端到端测试。

| 测试 | 内容 | 数量 |
|------|------|------|
| `float_basic` | 浮点变量、赋值、打印 | 1 |
| `float_arith` | 浮点四则运算、f32 数组（矩阵） | 1 |
| `float_cmp` | 浮点比较与分支 | 1 |
| `float_cast` | `i32 as f32` / `f32 as i32` 互转 | 1 |
| `float_func` | 浮点参数传递与返回值 | 1 |

**验收结果**：`cargo test --features float float_` → **5 passed**；主线 `cargo test` → 29 passed（`long_code2` 见 §六）；`cargo build` / `cargo build --features float` **无警告**。

**本次 Lab4 的边界**：只涉及后端（IR → AArch64 汇编）。具体是五块：浮点指令打印、浮点寄存器分配、浮点语句下降、AAPCS64 浮点参数/返回值约定。但实测中发现 lab3 遗留的 IR 形态与上游后端骨架的假设不一致，因此额外做了三处桥接改动（见 §五、§七）。

---

## 二、计划内改动（4 文件）

按 `asmt-4.md` 的设计，后端补齐浮点支持落在四个文件：

### 1. `printer.rs` —— 浮点指令打印 + caller-save FP 半段

把五条浮点指令从 `todo!()` 实现为真实发射：

| IR 指令 | 发射的 AArch64 | 说明 |
|---------|---------------|------|
| `FBinOp` | `fadd/fsub/fmul/fdiv s_d, s_n, s_m` | 单精度四则 |
| `FCmp` | `fcmp s_n, s_m` | 结果写 NZCV，由后续 `BCond` 消费 |
| `Scvtf` | `scvtf s_d, w_n` | i32 → f32，目标在 FP 库、源在 GPR 库 |
| `Fcvtzs` | `fcvtzs w_d, s_n` | f32 → i32（向零截断），方向相反 |
| `Fmov` | `fmov s_d, s_n` 或 `fmov s_d, w_n` | 寄存器源直接位拷贝；立即数源是 IEEE-754 位模式，先物化进整数 scratch `w16` 再位重解释进 FP 目标 |

并补齐 caller-save 的浮点半段：进入 `bl` 前把 `d18`–`d25` 以 `d` 对压栈（`stp d24,d25` … `stp d18,d19`），返回后逆序恢复，保持 sp 16 字节对齐。

**caller-saved 是什么**
- AAPCS64 规定有些寄存器是 caller-saved，也就是：调用者自己负责保存。被调用函数可以随便改它们。
- 要么寄存器分配器避免把跨调用存活的值放在 caller-saved 寄存器，要么在调用点包裹保存/恢复。
- bl (branch with link)，此处一般是

### 2. `register_allocator.rs` —— 双干扰子图染色

AArch64 的整数库（`x`）和浮点库（`s`/`d`）是**物理上独立**的两组寄存器，跨库的两个 vreg 永远不会争用同一个物理寄存器。因此把干扰图按寄存器类**拆成两个子图**分别染色：

- 整数 vreg → 子图染色到 `x8`–`x15`（`ALLOCATABLE_REGS`）
- 浮点 vreg → 子图染色到 `s18`–`s25`（`ALLOCATABLE_FPRS`）

新增 `restrict_to(members)` 构造由某一类 vreg 诱导的子图（丢弃跨类边），让度数计算与 spill 决策只针对同类邻居；`simplify` / `select` / `color` 改为接收 `num_colors` 与 `pool` 参数，不再硬编码整数池。浮点 spill / reload 走 FP scratch 对 `s16` / `s17`（与整数 `x16`/`x17` 共用编号但物理独立）。同时实现 5 条浮点指令的 `rewrite_*`（`FBinOp`/`FCmp`/`Scvtf`/`Fcvtzs`/`Fmov`），把虚拟寄存器重写为物理寄存器并插入必要的 spill/reload。

### 3. `function_generator.rs` —— 浮点语句下降

- `emit_fbiop` / `emit_fcmp` / `emit_sitofp` / `emit_fptosi`：把 IR 的浮点语句下降为对应 `Instruction`。浮点操作数先经 `lower_float_to_reg` 物化进 FP 寄存器，整数源经 `lower_int_to_reg`。
- `emit_fpr_arg`：AAPCS64 把 f32 实参放进 `s0`–`s7`，用 `fmov s{idx}, src` 落位（shim）。
- phi 拷贝：f32 的 phi 并行拷贝下降为 `fmov`（而非整数 `mov`）。

### 4. `aarch64.rs` —— `handle_arguments` 的 Fpr 入口臂

函数序言里把到达的 f32 形参从其 `s_` 寄存器经 `fmov` 抬进目标 vreg（`ArgumentLocation::Fpr(n)` 臂）。

**汇编核对**：生成的 `fadd` 序列与文档示例完全一致 —— caller-save FP 半段（`stp d18,d19`）、FP 溢出到帧槽（`stur s16, [x29,#-4]`）、`fcvtzs` 转换、浮点常量物化（`fmov s0, w16`）均正确发射。

vreg和物理reg的迁移点如下：
```
进入函数：ABI 物理寄存器 -> 本函数 vreg
调用别人：本函数 vreg -> ABI 物理寄存器
函数返回：本函数 vreg -> ABI 返回寄存器
调用结束：ABI 返回寄存器 -> 本函数 vreg
```

---

## 三、特殊说明：为什么 f32 会"先住在内存里"——SSA 与可变变量的根本矛盾

> 这是本次 Lab4 最重要的一个认知点，单独记录。

`mem2reg`（memory-to-register）这个 pass 的工作叫**提升（promotion）**：把存放在内存里、靠 `load`/`store` 访问的局部变量，转换成直接在寄存器/SSA 值之间流动的数据。问题是——**为什么前端一开始要把变量放进内存？**

答案不是"前端图省事"，而是 **SSA 与可变变量之间有根本矛盾**：

- SSA（Static Single Assignment）的核心约束是 **每个值只被定义（赋值）一次**。
- 而源语言里的局部变量是**可变**的，`x = x + 2` 要求同一个 `x` 被重新赋值。

这两者直接冲突。前端面对冲突有两条路：

1. **前端自己直接构造 SSA**：一边生成 IR 一边追踪"`x` 现在对应哪个 SSA 值"，并在分支汇合处自己插 phi。这需要在前端实现一套 SSA 构造算法（如 Cytron 支配边界算法），复杂且易错。
2. **先把可变变量塞进内存**（`alloca` + `load`/`store`）：内存单元没有"只赋值一次"的限制，可以反复 `store`。前端因此完全不必理会 SSA，线性地翻译每条语句；把"构造 SSA"这件难事统一交给 `mem2reg` 一个独立 pass 做对一次。

绝大多数编译器（LLVM 即典型）选第 2 条。这**不是偷懒，而是工程上更干净的关注点分离**：前端只负责正确翻译语义，SSA 构造集中在一个 pass 里做对。`alloca` 在这里扮演的角色是"一个语义上可变的容器"，专门用来绕开 SSA 的单赋值约束；随后 `mem2reg` 再把这些内存变量重新拆解成 SSA 值 + phi。

```
源码        let x = 1;  x = x + 2;
              │
前端（线性翻译，可变变量塞进内存）
              ▼
   %x = alloca i32
   store i32 1, %x          ← 内存单元可反复写，绕开 SSA 单赋值约束
   %t = load i32, %x
   %t2 = add i32 %t, 2
   store i32 %t2, %x
              │
mem2reg（提升：内存 → SSA + phi）
              ▼
   %x0 = 1
   %x1 = add i32 %x0, 2     ← 干净的数据流，分支汇合处用 phi 选源
```

**这正是 Lab4 那个坑的根源**：lab3 的前端按这套设计把 f32 局部变量也 `alloca` 进了内存，但本仓库的 `mem2reg` 当时**只认 i32**，没人去把 f32 提升回 SSA。于是浮点值永远停在 `load float`/`store float` 形态，到不了后端期望的 `fadd %r2, %r0, %r1`。详见下一节。

---

## 四、计划外的发现：lab3 的 f32-在内存 与 上游后端骨架不一致

原计划基于一个假设：**f32 到达后端时已是 SSA 形式**（文档示例都是 `%r2 = fadd float %r0, %r1`）。实测推翻了它：

- 本仓库 lab3 的 IR 生成把 f32 局部变量放进了内存（`alloca`/`store`/`load float`），而 `mem2reg` 只提升 i32。
- 对比 `upstream/assign4-latest`：上游骨架的 `mem2reg` 与 `layout` **同样不处理 f32**。这说明上游 asmt-3 参考实现是**直接生成 SSA 形式的 f32**，而本仓库 lab3 没有做到。

也就是说，问题根源在 lab3 留下的"f32-在内存"与"上游后端骨架期望 SSA f32"之间的鸿沟，lab4 只是第一个踩到它的人。

---

## 五、为打通而做的 3 处计划外桥接改动

| 文件 | 改动 | 原因 |
|------|------|------|
| `mem2reg.rs` | 标量提升从「仅 i32」扩展到「i32 + f32」，候选集合从 `HashSet<LocalId>` 改为 `HashMap<LocalId, Dtype>` 保留 pointee 类型，让插入的 phi 携带真实 `dtype` | f32 局部变量需要被提升到 SSA，否则到不了后端；f32 phi 必须带 `float` 类型，后端才能把 phi 拷贝下降为 `fmov` 而非整数 `mov`。这让 `compute` 的循环累加正确提升为 `phi float`（命中文档 §3.6） |
| `layout.rs` | `size_align_of` / `size_align_of_member` 增加 `F32 → (4,4)` | 供 f32 数组的 `gep` 计算元素尺寸（`float_arith` 的矩阵） |
| `function_generator.rs` `emit_store` | 存储浮点常量时用 `Fmov` 而非 `Mov` | `mov s18, #...` 是非法指令；FP 寄存器只能经 `fmov` 物化立即数（按 IEEE-754 位模式解释） |

这三处都很局部、风险低，没有触碰任何整数路径。

---

## 六、`long_code2` 编译期栈溢出（lab3 遗留，与 lab4 无关）

`long_code2` 在改动前的干净树上同样失败，已在 `reports/lab3-troubleshooting-stack-overflow.md` 记录。

- **现象**：teac 编译器自身 `thread 'main' has overflowed its stack`。
- **原因**：编译器多个阶段（解析、IR 生成、mem2reg、寄存器分配）对程序结构递归遍历；`long_code2` 输入特别大，递归深度超过 Rust 主线程默认 8 MiB 栈。这是编译器进程自身的原生线程栈溢出，与生成程序的栈帧无关。
- **修复**：在 `src/main.rs` 把整条编译流水线放到一个 **512 MiB 栈**的 worker 线程上运行（`std::thread::Builder::new().stack_size(...).spawn(run)`），递归深度随输入规模增长即可，不再被默认栈卡死。
- **结果**：`long_code` 与 `long_code2` 均通过。

> 注意：若后续某次溢出其实是某个 pass 的**无限递归 bug**（而非深度大），再大的栈也救不了 —— 那种情况要查具体 pass 的递归终止条件。当前 `long_code2` 属于"深但有限"，加栈是对的。

---

## 七、遗留决策（待定）

§五的 3 处桥接是"在 lab3 的 IR 形态上打补丁"。当前方案能让 5 个浮点测试全绿、且与文档行为一致，但偏离了上游设计。两条路：

1. **保留现状**：`mem2reg` 兼管 f32 提升。优点：已全绿、改动小；缺点：与上游分叉，f32 始终走"先入内存再提升"的弯路。
2. **回退桥接，改在 lab3 的 IR 生成里直接产出 SSA 形式的 f32**：更贴近上游设计，后端无需特殊处理内存里的浮点；缺点：要改 lab3 IR 生成，工作量与回归风险更大。

当前采用方案 1。

---

## 八、改动文件清单

| 文件 | 性质 |
|------|------|
| `src/asm/aarch64/printer.rs` | 计划内：浮点指令打印 + caller-save FP 半段 |
| `src/asm/aarch64/register_allocator.rs` | 计划内：双干扰子图染色 + 5 条浮点重写 |
| `src/asm/aarch64/function_generator.rs` | 计划内：浮点语句下降、AAPCS64 shim、phi 拷贝；计划外：`emit_store` 浮点常量用 `Fmov` |
| `src/asm/aarch64.rs` | 计划内：`handle_arguments` 的 Fpr 入口臂 |
| `src/opt/mem2reg.rs` | 计划外：i32+f32 提升、phi 携带真实 dtype |
| `src/asm/common/layout.rs` | 计划外：`F32 → (4,4)` |
| `src/main.rs` | lab3 遗留修复：编译流水线移到 512 MiB 栈线程 |
