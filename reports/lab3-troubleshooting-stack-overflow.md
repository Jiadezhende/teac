# 排错记录：Lab3 引入浮点后 `long_code2` 栈溢出

> 一句话：在**热递归路径**上内联了新逻辑，单帧体积变大，乘以约 4000 的递归深度把 8 MB 主线程栈撑爆。修法是把递归帧压回最小，而非加大栈。

---

## 一、现象

补完 Lab3 全部逻辑后，`cargo test` 中其余用例通过，唯独 `long_code2` 崩溃：

```
thread 'long_code2' has overflowed its stack
fatal runtime error: stack overflow
```

- 只在 `long_code2` 出现，其它 30+ 用例正常。
- 与浮点 / for 的 feature 开关无关——纯 i32 路径也会崩。
- 崩溃点在递归调用链里（`type_of_arith_expr` 与 `handle_arith_biop_expr` 互相递归），不是某条具体指令。

## 二、复现与定位

测试用例：[`tests/long_code2/long_code2.tea`](../tests/long_code2/long_code2.tea)（12410 行）。它的结尾是**一条**约 4000 项相加的单一表达式：

```
ans = a[a0].array[b0] + a[a1].array[b1] + … + a[a3999].array[b3999];
```

在 AST 中，这条表达式是一棵约 **4000 层深**的 `ArithBiOpExpr` 树（每个 `+` 是一个二元节点）。编译器有两处对它做递归下降：

| Pass | 函数 | 递归形态 |
|------|------|----------|
| 类型推断 | `type_of_arith_expr` ([type_infer.rs:456](../src/ir/gen/type_infer.rs#L456)) | 对 `biop.left` / `biop.right` 自递归 |
| IR 生成 | `handle_arith_biop_expr` ([function_gen.rs:905](../src/ir/gen/function_gen.rs#L905)) | `handle_arith_biop_expr → handle_arith_expr → handle_arith_biop_expr` |

每深入一层表达式就多压一个栈帧，深度约 4000。

## 三、根因

### 3.1 栈预算被深度放大

主线程默认栈 **8 MB**。平摊到 4000 层，每帧预算只有 `8 MB / 4000 ≈ 2 KB`。i32-only 的老代码每帧足够小，**"刚好够用"**——这是一个本就贴着上限的脆弱状态。

### 3.2 我的改动如何越过红线

Lab3 给这两个递归函数内联了浮点路径。要害在于 Rust 的栈帧布局：

> **一个函数的栈帧会为它的全部局部变量（含各 `match`/`if` 分支里的临时量）的并集预留空间，与运行时实际走哪条分支无关。**

也就是说，即便某次调用走的是 i32 分支，f32 分支里那些较大的局部——`Dtype`（带 `Box`/`Rc` 的枚举变体）、`Operand`（大枚举）、强转后的 `left`/`right`、`dst`——**仍然在每一帧都占地**。单帧多出几十到上百字节，乘以 4000 的深度，总量就突破了 8 MB。

直觉陷阱：这两个函数本身看起来人畜无害，"只是多了一个 if 分支"。但它们处在被深度放大约 4000 倍的热路径上，**每多 1 字节，就是多 ~4 KB 总栈**。

## 四、修复

核心思路：**不动递归结构，只把存活于递归之上的局部变量压到最小。**

### 4.1 `type_of_arith_expr`：跨递归只持 `bool`

```rust
// 修复后
ast::ArithExprInner::ArithBiOpExpr(biop) => {
    let left_is_f32  = self.type_of_arith_expr(&biop.left)?  == Dtype::F32;
    let right_is_f32 = self.type_of_arith_expr(&biop.right)? == Dtype::F32;
    if left_is_f32 || right_is_f32 { Ok(Dtype::F32) } else { Ok(Dtype::I32) }
}
```

关键：递归返回的 `Dtype` 是**当条语句结束即析构**的临时量；真正跨越**第二次（更深）递归**而存活的，只有 1 字节的 `bool`，而不是整个 `Dtype`。
若写成 `let left = self.type_of_arith_expr(&biop.left)?;`（一个 `Dtype` 局部），那么 `left` 会在第二次递归调用期间一直占着帧——这正是要避免的。

### 4.2 `handle_arith_biop_expr`：把发射逻辑抽到非递归辅助

```rust
// 修复后：递归函数只保留两个操作数 + 一次叶子调用
fn handle_arith_biop_expr(&mut self, expr: &ast::ArithBiOpExpr) -> Result<Operand, Error> {
    let left  = self.handle_arith_expr(&expr.left)?;
    let right = self.handle_arith_expr(&expr.right)?;
    self.emit_arith_biop(&expr.op, left, right)   // ← 非递归
}

// 分支繁重、局部众多的发射逻辑搬到独立函数
fn emit_arith_biop(&mut self, op: &ast::ArithBiOp, left: Operand, right: Operand)
    -> Result<Operand, Error>
{
    if matches!(left.dtype(), Dtype::F32) || matches!(right.dtype(), Dtype::F32) {
        let left = self.coerce_to_f32(left);
        let right = self.coerce_to_f32(right);
        let dst = Operand::from(self.fresh_local(Dtype::F32));
        self.emit_fbiop(FloatBinOp::from(op), left, right, dst.clone());
        Ok(dst)
    } else {
        let dst = Operand::from(self.fresh_local(Dtype::I32));
        self.emit_biop(ArithBinOp::from(op), left, right, dst.clone());
        Ok(dst)
    }
}
```

`emit_arith_biop` 是 `handle_arith_biop_expr` 在**两次递归都已返回之后**才调用的**叶子调用**——它的胖帧永远不会被 4000 层叠起来，每次只在栈顶短暂出现一个。

## 五、为什么有效（栈形态对比）

对左倾树 `((…((a0+a1)+a2)+…)+a3999)`，递归在 `node.left` 上一路下探。到达最深处时：

**修复前**——4000 个 `handle_arith_biop_expr` 帧叠在栈上，每帧都预留了 f32 分支的全部局部：

```
[ handle_biop(root)   | left,right,dst,coerced_l,coerced_r … ]  ← 胖
[ handle_biop(...)     | left,right,dst,coerced_l,coerced_r … ]  ← 胖
        ⋮  × 4000                                                  ⋮
峰值 ≈ 4000 × 胖帧  → 溢出
```

**修复后**——4000 个递归帧只剩 `left`/`right`；胖的发射逻辑搬出递归，只在解栈时作为叶子逐个出现：

```
[ handle_biop(root)   | left,right ]   ← 瘦
[ handle_biop(...)     | left,right ]   ← 瘦
        ⋮  × 4000                          ⋮
[ emit_arith_biop(...) | dst,coerced … ]  ← 唯一的胖帧，仅 1 层
峰值 ≈ 4000 × 瘦帧 + 1 × 胖帧  → 通过
```

净效果：递归帧甚至比改动**之前**更小，`long_code2` 恢复通过。

## 六、验证

```
cargo test                              → 30 passed   (主线 i32 端到端，零回归)
cargo test --features float             → 35 passed
cargo test --features for-loop          → 35 passed
cargo test --features float,for-loop    → 40 passed
```

`long_code2` 在所有 feature 组合下通过；其余用例无回归。

## 七、备选方案与取舍

| 方案 | 为何不选 |
|------|----------|
| 加大栈 / 开大栈子线程跑编译 | 治标不治本，只是把红线后移；测试在主线程跑，且本质问题（帧过大）仍在，换个更深的用例又会崩。 |
| 把递归改写成显式堆栈（迭代） | 改动面大、可读性差；本例只需瘦身递归帧即可解决，属过度工程。 |
| **瘦身递归帧（采用）** | 保留递归的可读性，仅搬运局部变量；零额外依赖，且帧比原来更小。 |

## 八、经验法则

1. **热递归路径上，每个字节都会被递归深度放大。** 在这种函数里加局部变量前，先想"× 深度"是多少。
2. **跨递归调用存活的状态要压到最小**：能用 `bool` 就别留 `Dtype`，让富类型在进入下一层递归前就析构。
3. **把分支繁重 / 局部众多的逻辑抽到非递归辅助函数**——叶子调用的胖帧不会被叠加，不计入"× 深度"。
4. Rust 栈帧按**所有分支局部的并集**预留，新增的冷分支也会拖累热路径的每一帧。
5. "刚好够用"的资源是脆弱信号：`long_code2` 本就贴着 8 MB 上限，任何无意的帧增长都会让它先崩——它实际上是一个很好的栈预算回归哨兵。
