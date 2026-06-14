# Lab4 实验报告

## 一、实验目标

Lab4 是编译器的**后端收尾**：从已经正确的 IR 出发产出 AArch64 汇编，核心工作是**把虚拟寄存器（vreg）正确分配到物理寄存器**，并在此基础上补齐 `f32` 浮点的整条下降路径，使浮点运算一路从 IR 走到合法汇编并通过端到端测试。

| 测试 | 内容 | 数量 |
|------|------|------|
| `float_basic` | 浮点变量、赋值、打印 | 1 |
| `float_arith` | 浮点四则运算、f32 数组（矩阵） | 1 |
| `float_cmp` | 浮点比较与分支 | 1 |
| `float_cast` | `i32 as f32` / `f32 as i32` 互转 | 1 |
| `float_func` | 浮点参数传递与返回值 | 1 |

**验收结果**：`cargo test --features float float_` → **5 passed**；主线 `cargo test` → 29 passed（`long_code2` 见 §八）；`cargo build` / `cargo build --features float` **无警告**。

**本次边界**：只涉及后端（IR → AArch64 汇编），计划内是五块：浮点指令打印、双干扰图寄存器分配、浮点语句下降、AAPCS64 浮点参数/返回值约定、caller-save 浮点半段。实测中发现 lab3 遗留的 IR 形态与上游后端骨架的假设不一致，因此额外做了三处桥接改动（见 §六、§七）。

---

## 二、后端全流程

| 阶段 | 做什么 | 入口 |
|------|--------|------|
| 入口 shim | 按 AAPCS64 把入参从 `x_`/`s_`/栈提取到 vreg | `handle_arguments` |
| 指令选择 | 遍历 IR 基本块，下降为带 vreg 的 `Instruction` 流，并完成 phi 下降 | `FunctionGenerator::generate` |
| 寄存器分配 | 活跃性分析 + 干扰图染色 + spill，重写为物理寄存器 | `RegisterAllocator::run` |
| 汇编打印 | 把 `Instruction` 流打印为汇编文本 | `AsmPrinter::emit_inst` |

vreg 与物理寄存器之间的迁移点贯穿全流程：

```
进入函数：ABI 物理寄存器 -> 本函数 vreg      （handle_arguments）
调用别人：本函数 vreg     -> ABI 物理寄存器   （emit_call 落参）
函数返回：本函数 vreg     -> ABI 返回寄存器
调用结束：ABI 返回寄存器   -> 本函数 vreg
```

---

> 下面三节按后端流水线顺序展开：指令选择（`function_generator`）→ 寄存器分配（`register_allocator`）→ 汇编打印（`printer`）。caller-saved 浮点保护是横跨三层的一条线，分别落在它在每一层的真实归属：调用点包裹（function_generator）→ 池定义（register_allocator）→ 半段发射与门控（printer）。

## 三、`function_generator.rs` 与 `aarch64.rs`：指令选择与浮点下降

后端的第二阶段，把 IR 基本块下降为带 vreg 的 `Instruction` 流。

### 3.1 浮点语句下降

- `emit_fbiop` / `emit_fcmp` / `emit_sitofp` / `emit_fptosi`：把 IR 浮点语句下降为对应 `Instruction`。浮点操作数先经 [`lower_float_to_reg`](../src/asm/aarch64/function_generator.rs#L740) 物化进 FP 寄存器，整数源经 `lower_int_to_reg`。
- phi 拷贝：f32 的 phi 并行拷贝下降为 `fmov`（而非整数 `mov`）。

### 3.2 立即数 float 的降级决策

`lower_float_to_reg` 遇到 `FloatConst` 时，分配一个 fresh vreg 并产出 `Fmov { dst, src: Immediate(f32_bits) }`——即「选 `Fmov` 而非 `Mov`、把常量按 IEEE-754 位模式塞进 Immediate 操作数」，因为 `mov s18, #…` 往 FP 寄存器写值本身就无意义。`emit_store` 存浮点常量同样改用 `Fmov`（§七 那处桥接）。

> 这一层只决定「用 `Fmov` 携带位模式立即数」。至于「任意立即数还得经 `w16` 中转才能真正进 FP 寄存器」，是 printer 序列化时的事（见 §五）——两层各管一段。

### 3.3 AAPCS64 shim（vreg ↔ ABI 物理寄存器）

- 入口 shim：`aarch64.rs` 的 `handle_arguments` 增加 `ArgumentLocation::Fpr(n)` 臂，函数序言里把到达的 f32 形参从其 `s_` 寄存器经 `fmov` 抬进目标 vreg。
- 出口 shim：`emit_fpr_arg` 把 f32 实参放进 `s0`–`s7`，用 `fmov s{idx}, src` 落位。

### 3.4 emit_call：调用点包裹

先说概念。按调用约定寄存器分两类：

- **caller-saved**：调用前如果调用者还需要里面的值，就必须自己保存；被调用函数可随便改。
- **callee-saved**：被调用函数若要用，必须自己保存并恢复。

「caller-saved 浮点池」就是一组**调用者负责保护**的浮点寄存器。它**没有跨调用的覆盖约束**：若某个浮点临时值跨越了一个函数调用点，它就不能天真地一直待在 caller-saved 浮点寄存器里，除非调用点前后有保存/恢复。考虑：

```c
double x = a * b + c;
foo();              // x 不跨调用 → 放 caller-saved 寄存器即可，无需保护
```

```c
double x = a * b + c;
foo();
return x + 1.0;     // x 跨过 foo()，而 foo 可能改写 caller-saved 寄存器
```

后者编译器必须包裹：

```
store x, [栈槽]
call  foo
load  x, [栈槽]
```

这就是「调用点包裹」。保存数据要么落 callee-saved 浮点寄存器、要么落栈槽，取决于重新加载的成本。实现在 `emit_call` 里，在每个 `Bl` 前后插入保存/恢复伪指令——真正的 `stp/ldp` 文本由 printer 在 §五 发射：

- `push(Instruction::SaveCallerRegs)`
- `push(Instruction::Bl { .. })`
- `push(Instruction::RestoreCallerRegs)`

---

## 四、`register_allocator.rs`：双干扰图染色

后端第三阶段，把 vreg **无冲突地**分配到物理寄存器。

### 4.1 为什么要双图

AArch64 里整数值用 GPR（`w0/x0` … `w30/x30`），浮点值用 FPR（`s0` … `s31`），这两套**物理独立**——`x8` 和 `s8` 编号都叫 8 但不是同一个寄存器，跨库的两个 vreg 永远不会争用同一物理寄存器。因此干扰图按寄存器类拆成两个子图分别染色：

- 整数 vreg → 染到 `x8`–`x15`（`ALLOCATABLE_REGS`）
- 浮点 vreg → 染到 `s18`–`s25`（`ALLOCATABLE_FPRS`）

实现上**统一建立一次干扰图**，再用 `restrict_to(members)` 构造由某一类 vreg 诱导的子图（丢弃跨类边），让度数计算与 spill 决策只针对同类邻居。`simplify` / `select` / `color` 改为接收 `num_colors` 与 `pool` 参数，不再硬编码整数池。

### 4.2 caller-saved 浮点池定义

浮点池就在这一层定下，[register_allocator.rs:24](../src/asm/aarch64/register_allocator.rs#L24)：

```rust
const ALLOCATABLE_FPRS: [u8; NUM_COLORS] = [18, 19, 20, 21, 22, 23, 24, 25];
```

即 AAPCS64 caller-saved 区段的 `s18`–`s25`。它与整数池 `x8`–`x15` 对称，也与 FP scratch 对（`s16`/`s17`）、AAPCS64 参数/返回寄存器（`s0`–`s7`）保持不相交。染到这里的浮点值若跨 `bl` 存活，由调用点的 `SaveCallerRegs`/`RestoreCallerRegs` 包裹（§三、§五）保护，而非被调用方。

### 4.3 活跃性分析（gen/kill）

第一步是计算**每条指令执行后剩余活跃的 vreg**。`build_gen_kill` 给每条指令建立 gen/kill：

- `gen` = 指令读取的 vreg，需要在入口处活着；
- `kill` = 指令定义的 vreg，旧值被覆盖，旧 vreg 不需要从入口活到出口。

### 4.4 着色

着色就是「给 vreg 分配物理寄存器」。干扰图里：

- **点** = vreg
- **边** = 两个 vreg 生命周期重叠，不能用同一物理寄存器
- **颜色** = 一个可用物理寄存器

本仓库用经典的 **Chaitin-Briggs 乐观染色**，分两趟：

- **`simplify`（拆解，建栈）**：反复摘掉「度数 < 颜色数」的节点压栈——这种节点必然有色可染，先放一边。当图里只剩高度数节点时，挑度数最大的那个当 **potential spill**（仅是「可能溢出」，并不立刻判溢出），同样压栈、继续拆。`pick_node` 就是这个选择器。
- **`select`（出栈，上色）**：逆序弹栈，给每个节点分配一个**邻居没用过的最小颜色**。乐观就在这里兑现：一个 potential spill 出栈时，它的高度数邻居未必都占满了所有颜色，往往仍能染上色；只有当确实无色可用时，才**真正溢出**到 spill 列表。最后 `spilled` 按「potential spill 优先」排序，让真正难染的先拿到栈槽。

### 4.5 spill / reload：真正溢出的节点怎么办

select 判定真正溢出的 vreg，不再有物理寄存器常驻，改为**在帧上分配一个 spill 槽**（`frame.alloc_spill(size)`），值平时住在内存里。重写阶段（`InstRewriter`）逐条指令把它接回物理寄存器：

- **读侧 reload**：指令用到一个 spilled 源时，先 `ldr` 把它从槽里载入一个 **scratch 寄存器**再参与运算（`load_src_reg` → `emit_spill_load`）。
- **写侧 spill**：指令的目标是 spilled vreg 时，先把结果算进 scratch，再 `str` 写回它的槽（`write_to_dst` → `emit_spill_store`）。

scratch 是专门留出、永不参与染色的寄存器对：整数 `SCRATCH0`/`SCRATCH1` = `x16`/`x17`（`w16`/`w17`），浮点 `F_SCRATCH0`/`F_SCRATCH1` = `s16`/`s17`。一条指令最多两个源 + 一个目标，所以两个 scratch 够用；典型分工是 lhs 用 scratch0、rhs 用 scratch1，目标复用 scratch0（lhs 此时已被消费）。

几个要点：

- **双库各用各的 scratch**：浮点 spill/reload 走 `s16`/`s17`，与整数 `x16`/`x17` 共编号但物理独立。混库指令（`Scvtf`/`Fcvtzs`）GPR 侧用整数 scratch、FP 侧用 FP scratch，两者天然不冲突。
- **地址计算先占 scratch 的情况**：`Ldr`/`Str`/`Gep` 若需要先把 spilled 基址/索引载入 scratch0，则后续源/目标用 `scratch_after_base` 让出到 scratch1，避免覆盖刚算好的地址。

5 条浮点指令的 `rewrite_*`（`FBinOp`/`FCmp`/`Scvtf`/`Fcvtzs`/`Fmov`）都走上面这套读侧 reload / 写侧 spill 的统一机制，把 vreg 重写为物理寄存器并按需插入 `ldr`/`str`。

---

## 五、`printer.rs`：汇编序列化

后端最后阶段，把 `Instruction` 流**安全、合法地序列化成目标平台汇编文本**，对上游屏蔽掉所有指令编码范围与 ABI 帧布局的细节。本质是「数据结构 → 语句」的可视化。

### 5.1 浮点指令打印

把五条浮点指令从 `todo!()` 补全为真实发射：

| IR 指令 | 发射的 AArch64 | 说明 |
|---------|---------------|------|
| `FBinOp` | `fadd/fsub/fmul/fdiv s_d, s_n, s_m` | 单精度四则 |
| `FCmp` | `fcmp s_n, s_m` | 结果写 NZCV，由后续 `BCond` 消费 |
| `Scvtf` | `scvtf s_d, w_n` | i32 → f32，目标在 FP 库、源在 GPR 库 |
| `Fcvtzs` | `fcvtzs w_d, s_n` | f32 → i32（向零截断），方向相反 |
| `Fmov` | `fmov s_d, s_n` 或 `fmov s_d, w_n` | 寄存器源直接位拷贝；立即数源经整数 scratch（`w16`）中转 |

> **立即数 float 在这一层的坑**：`fmov s_d, #imm` 的立即数编码只支持极有限的 8-bit 模式，任意 IEEE-754 位模式无法直接编码。因此 `emit_fmov` 看到 `Fmov { src: Immediate(bits) }` 时，把 bits 经 `emit_mov_imm` 物化进整数 scratch `w16`，再 `fmov s_d, w16` 位重解释进 FP 目标。「该用 `Fmov` 而非 `Mov`」的决策在上游 function_generator（§三.2），printer 只负责把已经定型的 `Fmov` 合法序列化。

### 5.2 caller-save 半段发射与 FP 门控

§三的 `SaveCallerRegs`/`RestoreCallerRegs` 伪指令在这里变成真实的 `stp/ldp` 文本。「半区」不是代码里的实体（没有独立指令/字段/枚举），它只是 [printer.rs:688-720](../src/asm/aarch64/printer.rs#L688) 里 `emit_save_caller_regs` / `emit_restore_caller_regs` **一个函数内两段 `writeln!` 的划分**：

- **整数半区**：保存/恢复 GPR `x8`–`x15`，**无条件恒发**。
- **FP 半区**：保存/恢复 FPR `d18`–`d25`（即浮点池 `s18`–`s25` 的 64 位视角），**被 `if self.current_fn_uses_fp` 门控**。
  - 注：写 `d18` 是 64 位视角，分配器谈 `s18` 是 32 位，同一物理寄存器；f32 只用低 32 位（`s`），但用 `stp d` 把整个 64 位一起存更省事。

唯一载体就是那个 `if`：整数的 `writeln!` 在 `if` 外，FP 的在 `if` 里。恢复时顺序反过来——FP 半区在前（它最后压栈、要最先弹出）。

门控标志的来源是一条贯穿 `aarch64.rs` 与 printer 的链路：

```
stream_uses_fp（判定） → set_uses_fp（设标志） → emit_save/restore_caller_regs（按标志决定是否带 FP 半段）
```

- **判定**：`stream_uses_fp` 扫描整条指令流，只要出现 `FBinOp`/`FCmp`/`Scvtf`/`Fcvtzs`/`Fmov`，或宽度为 `S32` 的 `Mov`/`Ldr`/`Str`，就判为用到浮点。
- **设标志**：`handle_function` 算出 `func.uses_fp`，于 [aarch64.rs:153](../src/asm/aarch64.rs#L153) 调用 `printer.set_uses_fp(func.uses_fp)`。

**为什么整数半区永远发射、FP 半区却门控（成本权衡，不是正确性要求）**：

- 多存一个没用到的寄存器只浪费几条指令+栈空间，不会出错；漏存一个跨调用还活跃的寄存器才会出错。门控的本质是「能证明安全才省」。
- 几乎没有函数完全不用 `x8`–`x15`：纯浮点函数也要 GPR 算地址/存指针/循环计数/搬参数。给它做 `uses_int` 标志绝大多数判真、省不下，纯添复杂度 → 直接恒发最简单。
- FP 是真正可选的：大量函数完全不碰浮点。`uses_fp=false` 时没有任何浮点寄存器跨调用活跃，跳过一定安全，是常见且实打实的节省。

| 函数类型 | 整数半区 | FP 半区 |
|---|---|---|
| 整数函数 | 发射 | 跳过（`uses_fp=false`） |
| 浮点函数 | 发射 | 发射 |

**汇编核对**：生成的 `fadd` 序列与文档示例完全一致——caller-save FP 半段（`stp d18,d19`）、FP 溢出到帧槽（`stur s16, [x29,#-4]`）、`fcvtzs` 转换、浮点常量物化（`fmov s0, w16`）均正确发射。

---

## 六、特殊说明：为什么 f32 会“先住在内存里”——SSA 与可变变量的根本矛盾

> 本次 Lab4 最重要的认知点，单独记录。

`mem2reg`（memory-to-register）这个 pass 的工作叫**提升（promotion）**：把存放在内存里、靠 `load`/`store` 访问的局部变量，转换成直接在寄存器/SSA 值之间流动的数据。问题是——**为什么前端一开始要把变量放进内存？**

答案不是“前端图省事”，而是 **SSA 与可变变量之间有根本矛盾**：

- SSA（Static Single Assignment）的核心约束是 **每个值只被定义（赋值）一次**。
- 而源语言里的局部变量是**可变**的，`x = x + 2` 要求同一个 `x` 被重新赋值。

这两者直接冲突。前端面对冲突有两条路：

1. **前端自己直接构造 SSA**：一边生成 IR 一边追踪“`x` 现在对应哪个 SSA 值”，并在分支汇合处自己插 phi。需要在前端实现一套 SSA 构造算法（如 Cytron 支配边界算法），复杂且易错。
2. **先把可变变量塞进内存**（`alloca` + `load`/`store`）：内存单元没有“只赋值一次”的限制，可反复 `store`。前端因此完全不必理会 SSA，线性翻译每条语句；把“构造 SSA”统一交给 `mem2reg` 一个独立 pass 做对一次。

绝大多数编译器（LLVM 即典型）选第 2 条。这**不是偷懒，而是工程上更干净的关注点分离**：前端只负责正确翻译语义，SSA 构造集中在一个 pass 里做对。`alloca` 在这里扮演“一个语义上可变的容器”，专门绕开 SSA 的单赋值约束；随后 `mem2reg` 再把这些内存变量重新拆解成 SSA 值 + phi。

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

**这正是 Lab4 那个坑的根源**：lab3 的前端按这套设计把 f32 局部变量也 `alloca` 进了内存，但本仓库的 `mem2reg` 当时**只认 i32**，没人去把 f32 提升回 SSA。于是浮点值永远停在 `load float`/`store float` 形态，到不了后端期望的 `fadd %r2, %r0, %r1`。

---

## 七、计划外的发现：lab3 的 f32-在内存 与上游后端骨架不一致

原计划基于一个假设：**f32 到达后端时已是 SSA 形式**（文档示例都是 `%r2 = fadd float %r0, %r1`）。实测推翻了它：

- 本仓库 lab3 的 IR 生成把 f32 局部变量放进了内存（`alloca`/`store`/`load float`），而 `mem2reg` 只提升 i32。
- 对比 `upstream/assign4-latest`：上游骨架的 `mem2reg` 与 `layout` **同样不处理 f32**。这说明上游 asmt-3 参考实现是**直接生成 SSA 形式的 f32**，而本仓库 lab3 没有做到。

也就是说，问题根源在 lab3 留下的“f32-在内存”与“上游后端骨架期望 SSA f32”之间的鸿沟，lab4 只是第一个踩到它的人。

### 为打通而做的 3 处计划外桥接改动

| 文件 | 改动 | 原因 |
|------|------|------|
| `mem2reg.rs` | 标量提升从「仅 i32」扩展到「i32 + f32」，候选集合从 `HashSet<LocalId>` 改为 `HashMap<LocalId, Dtype>` 保留 pointee 类型，让插入的 phi 携带真实 `dtype` | f32 局部变量需被提升到 SSA，否则到不了后端；f32 phi 必须带 `float` 类型，后端才能把 phi 拷贝下降为 `fmov` 而非整数 `mov`。使 `compute` 的循环累加正确提升为 `phi float` |
| `layout.rs` | `size_align_of` / `size_align_of_member` 增加 `F32 → (4,4)` | 供 f32 数组的 `gep` 计算元素尺寸（`float_arith` 的矩阵） |
| `function_generator.rs` `emit_store` | 存储浮点常量时用 `Fmov` 而非 `Mov` | `mov s18, #...` 是非法指令；FP 寄存器只能经 `fmov` 物化立即数（按 IEEE-754 位模式解释） |

这三处都很局部、风险低，没有触碰任何整数路径。

---

## 八、`long_code2` 编译期栈溢出（lab3 遗留，与 lab4 无关）

`long_code2` 在改动前的干净树上同样失败，已在 [lab3-troubleshooting-stack-overflow.md](lab3-troubleshooting-stack-overflow.md) 记录。

- **现象**：teac 编译器自身 `thread 'main' has overflowed its stack`。
- **原因**：编译器多个阶段（解析、IR 生成、mem2reg、寄存器分配）对程序结构递归遍历；`long_code2` 输入特别大，递归深度超过 Rust 主线程默认 8 MiB 栈。这是编译器进程自身的原生线程栈溢出，与生成程序的栈帧无关。
- **修复**：在 `src/main.rs` 把整条编译流水线放到一个 **512 MiB 栈**的 worker 线程上运行（`std::thread::Builder::new().stack_size(...).spawn(run)`）。
- **结果**：`long_code` 与 `long_code2` 均通过。

> 注意：若后续某次溢出其实是某个 pass 的**无限递归 bug**（而非深度大），再大的栈也救不了。当前 `long_code2` 属于“深但有限”，加栈是对的。

---

## 九、遗留决策（待定）

§七的 3 处桥接是“在 lab3 的 IR 形态上打补丁”。当前方案能让 5 个浮点测试全绿、且与文档行为一致，但偏离了上游设计。两条路：

1. **保留现状**：`mem2reg` 兼管 f32 提升。优点：已全绿、改动小；缺点：与上游分叉，f32 始终走“先入内存再提升”的弯路。
2. **回退桥接，改在 lab3 的 IR 生成里直接产出 SSA 形式的 f32**：更贴近上游设计，后端无需特殊处理内存里的浮点；缺点：要改 lab3 IR 生成，工作量与回归风险更大。

当前采用方案 1。

---

## 十、改动文件清单

| 文件 | 性质 |
|------|------|
| `src/asm/aarch64/printer.rs` | 计划内：浮点指令打印 + caller-save FP 半段 |
| `src/asm/aarch64/register_allocator.rs` | 计划内：双干扰子图染色 + 5 条浮点重写 |
| `src/asm/aarch64/function_generator.rs` | 计划内：浮点语句下降、AAPCS64 shim、phi 拷贝；计划外：`emit_store` 浮点常量用 `Fmov` |
| `src/asm/aarch64.rs` | 计划内：`handle_arguments` 的 Fpr 入口臂、`stream_uses_fp` 判定 |
| `src/opt/mem2reg.rs` | 计划外：i32+f32 提升、phi 携带真实 dtype |
| `src/asm/common/layout.rs` | 计划外：`F32 → (4,4)` |
| `src/main.rs` | lab3 遗留修复：编译流水线移到 512 MiB 栈线程 |
