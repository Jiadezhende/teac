# Lab3 验收速读讲义

> 目的：从零到能讲清楚整个实现。**按本文顺序读一遍**，每一节都对到 `文件:行号`，
> 读完你能（1）说清数据怎么流，（2）翻到对应代码，（3）应付现场提问。

---

## 0. 30 秒电梯陈述（先背这段）

> "Lab3 是 **AST → LLVM IR** 这一段。我做了必做的 `f32` + `as`，三选一选了 `for` 循环。
> 整条链路是：源码先被 Lab1 解析成 AST，**Lab3 在两个地方动手**——
> 先在 `type_infer.rs` 给每个表达式**算出类型**（i32 还是 f32），
> 再在 `function_gen.rs` 按类型**生成 IR 指令**（整数走 `add`，浮点走 `fadd`，
> 类型不匹配就插 `sitofp`/`fptosi` 转换）。for 循环则是把高级循环**降级成 4 个基本块**。"

---

## 1. 先搞清楚：我的代码在整个编译器的哪一段

```
.tea 源码
   │  Lab1：Pest 词法/语法 → AST        （src/parser/*, src/ast/*，不是本次重点）
   ▼
  AST
   │  ★Lab3-A：类型推断  src/ir/gen/type_infer.rs   ← 给每个表达式标上 i32/f32
   ▼
  带类型的 AST
   │  ★Lab3-B：IR 生成   src/ir/gen/function_gen.rs ← 真正吐出 LLVM 指令
   ▼
  LLVM IR（内存里的 Stmt 列表）
   │  mem2reg 优化（已有，不是我写的）  ← 把 alloca/load/store 提升成 phi
   ▼
  .ll 文本   （src/ir/value.rs / types.rs / stmt.rs 负责"怎么打印成文本"）
   │  clang 编译 + 链接 std.c
   ▼
  可执行文件 → 运行 → 比对 .out
```

**记住这张图**：被问"你改了哪"时，照着 `type_infer → function_gen → (value/types/stmt 打印)` 三层说。

- **type_infer.rs**：只判断"这个表达式是什么类型"，不生成指令。
- **function_gen.rs**：核心，把 AST 语句/表达式翻译成 IR 指令。
- **value.rs / types.rs / stmt.rs**：IR 的"数据结构 + 怎么打印成 `.ll` 文本"。

---

## 2. 必做 Part A：f32 类型——从"加一个类型"开始

新增一个类型，要在 IR 三层都登记。按这个顺序看：

### 2.1 类型本身 — [types.rs:9](src/ir/types.rs#L9)
```rust
pub enum Dtype { Void, I1, I32, F32, ... }   // 新增 F32
// 打印成 LLVM 的 "float"（不是 "f32"！）
Dtype::F32 => write!(f, "float"),            // types.rs:51
```
还在返回类型白名单放行 F32（[types.rs:107](src/ir/types.rs#L107)），否则 `fn ... -> f32` 会被拒。

### 2.2 浮点常量 — [value.rs:107](src/ir/value.rs#L107)（★最容易被问的坑）
新增 `FloatConst` 操作数。**重点在它怎么打印**：
```rust
// value.rs:120
Dtype::F32 => f64::from(self.val as f32).to_bits(),  // 先 as f32 再回 f64
```
**为什么？**（背下面这句）
> "LLVM 里所有浮点常量都用 **64 位十六进制**写，连 `float` 也是。但如果直接写一个
> f32 表示不了的双精度位模式，LLVM 会**报错拒绝整个模块**。所以要先把值截成 f32
> 再扩回 f64，保证这个位模式在 f32 下是精确的。"

**现场证据**：`9.9` 在 [float_cast.tea](tests/float_cast/float_cast.tea) 里 → IR 是 `store float 0x4023CCCCC0000000`，这就是 9.9 round-trip 后的位模式。

### 2.3 浮点指令 — [stmt.rs:77](src/ir/stmt.rs#L77)
新增 4 条指令：`FBiOp`（fadd/fsub/fmul/fdiv）、`FCmp`（浮点比较）、`SIToFP`（i32→f32）、`FPToSI`（f32→i32）。
每条都要补三处：`Display`（打印）、`operands()`、`map_use_operands()`——**少一处优化 pass 就看不到这个操作数**。

### 2.4 emit 辅助 — [function.rs:304](src/ir/function.rs#L304)
`emit_fbiop / emit_fcmp / emit_sitofp / emit_fptosi`，就是"把指令塞进当前基本块"的薄封装。

---

## 3. 必做 Part B：类型推断——决定走整数路还是浮点路

IR 生成时要知道"`a + b` 该用 `add` 还是 `fadd`"，靠 type_infer 先算类型。

### 3.1 字面量和 as — [type_infer.rs:481](src/ir/gen/type_infer.rs#L481)
```rust
ast::ExprUnitInner::Float(_) => Ok(Dtype::F32),        // 浮点字面量就是 f32
ast::ExprUnitInner::Cast(cast) => {                     // x as T
    self.type_of_expr_unit(&cast.unit)?;                // 仍检查操作数（抓未定义变量）
    Ok(Dtype::from(cast.cast_to.as_ref()))              // 类型 = 目标类型 T
}
```

### 3.2 浮点的"传染性" — [type_infer.rs:461](src/ir/gen/type_infer.rs#L461)（要点）
```rust
if left == Dtype::F32 || right == Dtype::F32 { Ok(Dtype::F32) } else { Ok(Dtype::I32) }
```
> "只要二元运算有一边是 f32，结果就是 f32，整数那边后面会被隐式转上去。"

---

## 4. 必做 Part C：IR 生成——隐式转换是一张"边界清单"

这是必做的核心，也是最可能被追问的。**核心思想**：LLVM 强类型，i32 和 f32 不能混，所以凡是"类型可能穿越"的地方都要插转换指令。

### 4.1 三个转换原语 — [function_gen.rs:633](src/ir/gen/function_gen.rs#L633)
```rust
coerce_to_f32(op)        // i32 → f32，发 sitofp（已是 f32 就原样返回）
coerce_to_i32(op)        // f32 → i32，发 fptosi
coerce_to(op, target)    // 按 (源,目标) 派发，匹配/非标量则啥都不做
```
**先理解这三个，下面所有地方都只是在调它们。**

### 4.2 必须插转换的 6 个边界（背这张表，老师爱问"漏了会怎样"）

| # | 边界 | 例子 | 代码 |
|---|------|------|------|
| 1 | 变量定义 | `let x:f32 = 1;` 存 1.0 | function_gen.rs ~324 `coerce_to(pointee)` |
| 2 | 赋值 | `x = 1;`（x 是 f32） | [handle_assignment_stmt](src/ir/gen/function_gen.rs#L169) |
| 3 | return | f32 函数里 `return 0;` | [handle_return_stmt](src/ir/gen/function_gen.rs#L590) |
| 4 | 二元算术 | `1 + 2.0` → fadd | [handle_arith_biop_expr:931](src/ir/gen/function_gen.rs#L931) |
| 5 | 比较 | `a < 2.0` → fcmp | [handle_com_op_expr:671](src/ir/gen/function_gen.rs#L671) |
| 6 | as 表达式 | `x as f32` | [function_gen.rs:767](src/ir/gen/function_gen.rs#L767) |

> 漏了会怎样？→ "生成的 `.ll` 里 i32 和 float 混在一条指令上，**clang 直接拒绝编译**。"

### 4.3 return 为什么特殊 — [function_gen.rs:79](src/ir/gen/function_gen.rs#L79)
函数体里看不到自己的返回类型，所以在 `generate()` 开头把返回类型存进
`self.return_dtype`，`handle_return_stmt` 才能据此强转。

### 4.4 算术怎么分流 — [function_gen.rs:931](src/ir/gen/function_gen.rs#L931)
```rust
if 左或右是 f32 {
    两边 coerce_to_f32;  emit_fbiop(fadd...);  结果 f32
} else {
    emit_biop(add...);   结果 i32
}
```
比较同理（[handle_com_op_expr:684](src/ir/gen/function_gen.rs#L684)），但浮点用 **ordered 谓词** `oeq/olt/...`（[stmt.rs FCmpPredicate](src/ir/stmt.rs)）。
> 为什么 ordered？→ "ordered 表示有 NaN 时比较为假，符合 TeaLang 语义。"

### 4.5 现场看懂这段 IR（[float_cast.tea](tests/float_cast/float_cast.tea) 第一段）
```llvm
%r2 = sitofp i32 42 to float        ; let f = x as f32   → 边界#6/coerce_to_f32
%r3 = alloca float                  ; f 的栈槽（类型 float）
store float %r2, ptr %r3
%r4 = load float, ptr %r3
%r5 = fdiv float %r4, 0x4000000000000000  ; f / 2.0 → fadd 家族，常量 2.0 已 round-trip
...
%r8 = fptosi float %r7 to i32       ; half as i32  → coerce_to_i32
```
**讲解模板**：`x as f32` 触发 sitofp → 浮点除法用 fdiv、常量是 64 位 hex → `as i32` 触发 fptosi 截断。

---

## 5. 三选一：for 循环——把循环"降级"成基本块

### 5.1 一句话原理
> "高级语言的 `for` 没有对应的单条 IR 指令，要**拆成基本块 + 跳转**来表达。我用 4 个块。"

### 5.2 四基本块结构 — [handle_for_stmt:475](src/ir/gen/function_gen.rs#L475)（核心，画出来）
```
  preheader → test ←──────── incr
                ↓ i<end          ↑
              body ─────────────┘
                ↓ i>=end
              exit
```
- **preheader**：求值上下界，给 `i` 和上界各开一个栈槽，存初值，跳 test
- **test**：load i 和上界，`icmp slt`，真→body 假→exit
- **body**：循环体；**`continue`→incr，`break`→exit**（靠给 `handle_block` 传 incr/exit 两个 label 实现）
- **incr**：`i = i + 1`，跳回 test
- **exit**：出口

**最易被问**：continue 为什么跳 incr 不跳 test？
> "如果跳 test 就**跳过了 i+1**，i 永远不变成死循环。跳 incr 才能先自增再判断。"

### 5.3 为什么用栈槽 alloca 而不是直接 SSA/phi？
> "循环变量每轮都变，直接写 SSA 要手写 phi 很麻烦。我先用 `alloca`+`load`/`store`，
> 后面**已有的 mem2reg pass 会自动把栈槽提升成 phi 节点**，省事还不易错。"

**现场证据**（[for_basic](tests/for_basic/for_basic.tea) `for i in 0..10 { sum=sum+i }` 的 IR）：
```llvm
br label %bb1
bb1:                                      ; ← test 块
  %r39 = phi i32 [ 0, %main ], [ %r8, %bb3 ]   ; sum：mem2reg 生成的 phi
  %r40 = phi i32 [ 0, %main ], [ %r10, %bb3 ]  ; i
  %r5 = icmp slt i32 %r40, 10                   ; i < 10
  br i1 %r5, label %bb2, label %bb4
bb2:                                      ; ← body：sum = sum + i
  %r8 = add i32 %r39, %r40
  br label %bb3
bb3:                                      ; ← incr：i = i + 1
  %r10 = add i32 %r40, 1
  br label %bb1
bb4:                                      ; ← exit
```
> 讲：bb1=test、bb2=body、bb3=incr、bb4=exit；那两个 `phi` 就是我写的 alloca 被 mem2reg 提升的结果。

### 5.4 配套：循环变量作用域 — [process_for:340](src/ir/gen/type_infer.rs#L340)
type_infer 里 fork 一个环境，绑定 `i:i32`，处理完循环体后 `remove(i)` 再 merge 回来。
> "保证 `i` 只在循环体内可见，循环外用 `i` 会报未定义；而且循环体可能跑 0 次的语义也被 merge 正确处理。"

range 上下界支持 数字/变量/表达式/函数调用 四种，见 [handle_range_bound:527](src/ir/gen/function_gen.rs#L527)。

---

## 6. 跨切面：为什么后端也改了

新增 `Dtype::F32` / `Operand::FloatConst` / 4 条指令后，Rust 的 `match` 穷尽性会让
**aarch64 后端**（asmt-4 范围）编译不过，必须补分支。
> "我补齐了后端的穷尽匹配让它能编译，但 **float 的真正汇编实现下放到 Lab4**；
> Lab3 阶段 float 只走 `--emit ir` + clang 验证，不走汇编路径。"

---

## 7. 怎么验证的（被问"你测了吗"照这说）

| 测什么 | 命令 | 结果 |
|--------|------|------|
| 主线没被改坏 | `cargo test` | 30 全过 |
| Lab2 | `cargo test --features return-type-inference` | 35 全过 |
| **for 循环** | `cargo test --features for-loop` | 5 个经 aarch64+qemu 比对 .out 全过 |
| f32/as | `--emit ir` 对照 asmt-3.md 示例逐条核对 | 一致 |

**坦白点**：本机没装 clang，所以 float 的端到端（`--features float,asmt-tests-ir`）没在本机跑；
装上 clang 即可。for 是借 Lab4 的 qemu 路径**真端到端验证过**的。

---

## 8. 高频问答速记（考前扫一遍）

**Q：Lab3 你到底改了哪几个文件？**
A：核心两个——`type_infer.rs`（推类型）和 `function_gen.rs`（生成指令）；
配套 IR 三件套 `types.rs`/`value.rs`/`stmt.rs`（加类型、加常量、加指令）和 `function.rs`（emit 封装）。

**Q：i32 和 f32 怎么混着算的？**
A：type_infer 先判类型（浮点传染），function_gen 里有 `coerce_to` 系列，在定义/赋值/return/算术/比较/as 六个边界插 `sitofp`/`fptosi`。

**Q：浮点常量为什么是个长十六进制？为什么要 round-trip？**
A：LLVM 浮点常量统一 64 位 hex；不 round-trip 的话 f32 表示不了的位模式会被 LLVM 拒绝。（[value.rs:120](src/ir/value.rs#L120)）

**Q：for 循环几个块？continue/break 跳哪？**
A：4 块 test/body/incr/exit；continue→incr（保证 i 自增），break→exit。

**Q：那些 phi 是你写的吗？**
A：不是。我写的是 alloca+load/store，mem2reg 自动提升成 phi。

**Q：sitofp / fptosi 区别？**
A：sitofp = 有符号整数→浮点（i32→float）；fptosi = 浮点→有符号整数（float→i32，**截断**取整，9.9→9）。

---

## 9. 翻代码的最短路径（验收现场按需跳转）

1. 加类型：[types.rs:9](src/ir/types.rs#L9)
2. 浮点常量打印（坑）：[value.rs:120](src/ir/value.rs#L120)
3. 类型推断分流：[type_infer.rs:461](src/ir/gen/type_infer.rs#L461)
4. 转换原语：[function_gen.rs:633](src/ir/gen/function_gen.rs#L633)
5. 算术分流：[function_gen.rs:931](src/ir/gen/function_gen.rs#L931)
6. for 循环：[function_gen.rs:475](src/ir/gen/function_gen.rs#L475)
7. for 作用域：[type_infer.rs:340](src/ir/gen/type_infer.rs#L340)
