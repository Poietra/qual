//! Intra-procedural control-flow graphs (DESIGN §5.7).
//!
//! One [`ControlFlowGraph`] covers one function body. Basic blocks hold
//! straight-line work items ([`CfgStmt`]) and end in a [`Terminator`]
//! describing the outgoing edges. Handled constructs:
//!
//! - `if` / `elif` / `else` (two-way [`Terminator::Branch`]),
//! - `for` / `while` with `else` clauses, `break`, `continue`
//!   (loop-header blocks with back edges; `break` skips the `else` blocks
//!   exactly as at runtime),
//! - `return` and `raise` (terminators without successors),
//! - `try` / `except` / `else` / `finally`: a [`Terminator::Switch`]
//!   dispatches from the pre-`try` state into the body and into every
//!   handler, matching the conservative approximation used by the name
//!   resolver — the body may raise at any point, so a handler joined from
//!   the pre-`try` state never yields a *wrong certain* fact, only weaker
//!   maybe-facts. `finally` blocks join both paths. Early exits
//!   (`return` inside `try`) do not route through `finally`; this is a
//!   documented under-approximation of the `finally` *effects*, never of
//!   reachability.
//! - `with` / `async with` (context entries as [`CfgStmt::WithEnter`],
//!   body inline),
//! - `match` (a [`Terminator::Switch`] over the case blocks; the
//!   fall-through edge is dropped when the last case is irrefutable),
//! - comprehensions stay inside their containing statement as bounded
//!   single-evaluation summaries — they never contribute blocks or edges.
//!
//! Block IDs are deterministic: blocks are numbered in AST walk order, so
//! the same source always yields the same graph.

use rustpython_parser::ast;

/// Index of a basic block inside its [`ControlFlowGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub usize);

/// One straight-line work item inside a basic block.
#[derive(Debug, Clone, Copy)]
pub enum CfgStmt<'a> {
    /// A simple statement executed as written.
    Stmt(&'a ast::Stmt),
    /// An expression evaluated for effect only (a loop iterable or a
    /// `match` subject).
    Eval(&'a ast::Expr),
    /// A `with` context-manager entry (`context_expr` + optional binding).
    WithEnter(&'a ast::WithItem),
    /// The loop target is (re)bound at the start of an iteration.
    LoopTarget(&'a ast::Expr),
    /// Names captured by a `match` pattern are bound opaquely.
    PatternBind(&'a ast::Pattern),
}

/// What is known about a loop header's iteration count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopIteration {
    /// A literal `range(...)` iterable proves the trip count exactly
    /// (`range(3)` → 3; `range(5, 2)` → 0). The count is still only an
    /// *upper* bound on body executions — `break` / `return` / `raise`
    /// can end the loop early.
    Literal(i64),
    /// The iteration count is not statically known.
    Unknown,
}

/// The outgoing edges of a basic block.
#[derive(Debug, Clone)]
pub enum Terminator<'a> {
    /// Unconditional edge.
    Jump(BlockId),
    /// Two-way branch. `test` is `None` for the implicit iteration
    /// condition of a `for` loop header.
    Branch {
        /// The branch condition, evaluated in this block.
        test: Option<&'a ast::Expr>,
        /// Successor when the condition holds (loop body for headers).
        then_block: BlockId,
        /// Successor when it does not (loop exit / `else` for headers).
        else_block: BlockId,
    },
    /// Multi-way dispatch: `try` bodies with their handlers, and `match`
    /// case lists. Every target is a possible successor.
    Switch {
        /// Possible successors in source order.
        targets: Vec<BlockId>,
    },
    /// `return`; carries the returned expression. No successors.
    Return(Option<&'a ast::Expr>),
    /// `raise`. No successors.
    Raise,
    /// Normal end of the function body. No successors.
    End,
}

/// One basic block.
#[derive(Debug, Clone)]
pub struct BasicBlock<'a> {
    /// This block's ID (its index in [`ControlFlowGraph::blocks`]).
    pub id: BlockId,
    /// Straight-line work items in execution order.
    pub stmts: Vec<CfgStmt<'a>>,
    /// Outgoing edges.
    pub terminator: Terminator<'a>,
    /// Number of enclosing loops (`for` / `while`) around this block.
    pub loop_depth: u32,
    /// Number of enclosing conditional regions (branch arms, loop bodies,
    /// handlers, `match` cases). `0` means the block runs on every path
    /// through the function.
    pub cond_depth: u32,
    /// `true` for `for` / `while` header blocks (join point of the entry
    /// edge and the back edge).
    pub is_loop_header: bool,
    /// For loop headers: what is known about the iteration count.
    pub iteration: LoopIteration,
    /// `true` when at least one enclosing loop provably iterates twice or
    /// more (a literal `range(...)` bound). Allocations in such blocks get
    /// [`crate::semantic::values::Cardinality::Many`]; other loop blocks
    /// only get `MaybeMany` (DESIGN §5.5).
    pub in_definite_loop: bool,
    /// Upper bound on how many times this block executes per run of the
    /// function body, from the enclosing loops' trip counts: `Some(1)`
    /// outside loops, the product of the literal `range(...)` counts when
    /// every enclosing loop has one (early exits can only reduce it), and
    /// `None` when any enclosing loop's trip count is unknown — never `1`
    /// for an unknown count (DESIGN §4.1: unknowns must not underestimate).
    /// Loop headers execute one extra time (the exit test).
    pub repetitions: Option<i64>,
}

/// The control-flow graph of one function body.
#[derive(Debug, Clone)]
pub struct ControlFlowGraph<'a> {
    /// Blocks indexed by [`BlockId`]; creation order equals AST walk order.
    pub blocks: Vec<BasicBlock<'a>>,
    /// The entry block (always `BlockId(0)`).
    pub entry: BlockId,
}

impl<'a> ControlFlowGraph<'a> {
    /// Builds the CFG of one function body.
    #[must_use]
    pub fn build(body: &'a [ast::Stmt]) -> Self {
        let mut builder = Builder::new();
        let entry = builder.new_block();
        builder.current = entry;
        builder.walk(body);
        builder.seal(Terminator::End);
        Self {
            blocks: builder.blocks,
            entry,
        }
    }

    /// Successor block IDs of `id`, in deterministic edge order.
    #[must_use]
    pub fn successors(&self, id: BlockId) -> Vec<BlockId> {
        match &self.blocks[id.0].terminator {
            Terminator::Jump(target) => vec![*target],
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
            Terminator::Switch { targets } => targets.clone(),
            Terminator::Return(_) | Terminator::Raise | Terminator::End => Vec::new(),
        }
    }

    /// Reverse postorder over the blocks reachable from the entry.
    ///
    /// For the structured graphs this builder produces, this order visits a
    /// branch's arms before the join block and a loop body before the loop
    /// exit, i.e. it matches program order with each loop body appearing
    /// once.
    #[must_use]
    pub fn reverse_postorder(&self) -> Vec<BlockId> {
        let mut visited = vec![false; self.blocks.len()];
        let mut postorder = Vec::new();
        self.postorder_from(self.entry, &mut visited, &mut postorder);
        postorder.reverse();
        postorder
    }

    fn postorder_from(&self, id: BlockId, visited: &mut [bool], out: &mut Vec<BlockId>) {
        if visited[id.0] {
            return;
        }
        visited[id.0] = true;
        // Visit successors in *reverse* edge order so that after the final
        // reversal the first edge (then-arm, loop body) comes first.
        for successor in self.successors(id).into_iter().rev() {
            self.postorder_from(successor, visited, out);
        }
        out.push(id);
    }

    /// Predecessor IDs of every block (index = block ID).
    #[must_use]
    pub fn predecessors(&self) -> Vec<Vec<BlockId>> {
        let mut preds = vec![Vec::new(); self.blocks.len()];
        for block in &self.blocks {
            for successor in self.successors(block.id) {
                preds[successor.0].push(block.id);
            }
        }
        preds
    }
}

struct LoopFrame {
    header: BlockId,
    /// Where `break` jumps: after the whole statement, skipping `else`.
    after: BlockId,
}

struct Builder<'a> {
    blocks: Vec<BasicBlock<'a>>,
    current: BlockId,
    loop_stack: Vec<LoopFrame>,
    loop_depth: u32,
    cond_depth: u32,
    definite_loops: u32,
    /// Running upper bound on per-body-run executions of blocks created
    /// now: the product of the enclosing loops' literal trip counts
    /// (`Some(1)` outside loops), `None` once any enclosing loop's count
    /// is unknown or the product overflows.
    repetition_bound: Option<i64>,
    /// Set once the current block ended in `return` / `raise` / `break` /
    /// `continue`; subsequent statements go into an unreachable block.
    finished: bool,
}

impl<'a> Builder<'a> {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            current: BlockId(0),
            loop_stack: Vec::new(),
            loop_depth: 0,
            cond_depth: 0,
            definite_loops: 0,
            repetition_bound: Some(1),
            finished: false,
        }
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len());
        self.blocks.push(BasicBlock {
            id,
            stmts: Vec::new(),
            terminator: Terminator::End,
            loop_depth: self.loop_depth,
            cond_depth: self.cond_depth,
            is_loop_header: false,
            iteration: LoopIteration::Unknown,
            in_definite_loop: self.definite_loops > 0,
            repetitions: self.repetition_bound,
        });
        id
    }

    fn push(&mut self, stmt: CfgStmt<'a>) {
        if self.finished {
            // Dead code after return/raise/break/continue: park it in a
            // fresh unreachable block so spans stay addressable.
            let unreachable = self.new_block();
            self.current = unreachable;
            self.finished = false;
        }
        let current = self.current;
        self.blocks[current.0].stmts.push(stmt);
    }

    /// Sets the current block's terminator unless it already ended.
    fn seal(&mut self, terminator: Terminator<'a>) {
        if self.finished {
            return;
        }
        let current = self.current;
        self.blocks[current.0].terminator = terminator;
    }

    fn finish_with(&mut self, terminator: Terminator<'a>) {
        self.seal(terminator);
        self.finished = true;
    }

    /// Continues in `block` (opening it as the new current block).
    fn resume_at(&mut self, block: BlockId) {
        self.current = block;
        self.finished = false;
    }

    fn walk(&mut self, stmts: &'a [ast::Stmt]) {
        for stmt in stmts {
            self.walk_stmt(stmt);
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per compound statement kind")]
    fn walk_stmt(&mut self, stmt: &'a ast::Stmt) {
        match stmt {
            ast::Stmt::If(inner) => {
                let then_block;
                let else_block;
                let join;
                {
                    self.cond_depth += 1;
                    then_block = self.new_block();
                    else_block = if inner.orelse.is_empty() {
                        None
                    } else {
                        Some(self.new_block())
                    };
                    self.cond_depth -= 1;
                    join = self.new_block();
                }
                self.seal(Terminator::Branch {
                    test: Some(&inner.test),
                    then_block,
                    else_block: else_block.unwrap_or(join),
                });
                let was_finished = self.finished;
                self.cond_depth += 1;
                self.resume_at(then_block);
                self.walk(&inner.body);
                self.seal(Terminator::Jump(join));
                if let Some(else_block) = else_block {
                    self.resume_at(else_block);
                    self.walk(&inner.orelse);
                    self.seal(Terminator::Jump(join));
                }
                self.cond_depth -= 1;
                self.finished = was_finished;
                if !was_finished {
                    self.resume_at(join);
                }
            }
            ast::Stmt::While(inner) => {
                self.build_loop(
                    None,
                    Some(&inner.test),
                    LoopIteration::Unknown,
                    &inner.body,
                    &inner.orelse,
                );
            }
            ast::Stmt::For(inner) => {
                self.push(CfgStmt::Eval(&inner.iter));
                self.build_loop(
                    Some(&inner.target),
                    None,
                    iteration_bound(&inner.iter),
                    &inner.body,
                    &inner.orelse,
                );
            }
            ast::Stmt::AsyncFor(inner) => {
                self.push(CfgStmt::Eval(&inner.iter));
                self.build_loop(
                    Some(&inner.target),
                    None,
                    LoopIteration::Unknown,
                    &inner.body,
                    &inner.orelse,
                );
            }
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    self.push(CfgStmt::WithEnter(item));
                }
                self.walk(&inner.body);
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    self.push(CfgStmt::WithEnter(item));
                }
                self.walk(&inner.body);
            }
            ast::Stmt::Try(inner) => {
                self.build_try(
                    &inner.body,
                    &inner.handlers,
                    &inner.orelse,
                    &inner.finalbody,
                );
            }
            ast::Stmt::TryStar(inner) => {
                self.build_try(
                    &inner.body,
                    &inner.handlers,
                    &inner.orelse,
                    &inner.finalbody,
                );
            }
            ast::Stmt::Match(inner) => self.build_match(inner),
            ast::Stmt::Return(inner) => {
                self.finish_with(Terminator::Return(inner.value.as_deref()));
            }
            ast::Stmt::Raise(_) => {
                // The raise expression itself is evaluated first.
                self.push(CfgStmt::Stmt(stmt));
                self.finish_with(Terminator::Raise);
            }
            ast::Stmt::Break(_) => {
                if let Some(frame) = self.loop_stack.last() {
                    let after = frame.after;
                    self.finish_with(Terminator::Jump(after));
                }
                // A break outside any loop is a syntax error Python already
                // rejected; ignore defensively.
            }
            ast::Stmt::Continue(_) => {
                if let Some(frame) = self.loop_stack.last() {
                    let header = frame.header;
                    self.finish_with(Terminator::Jump(header));
                }
            }
            // Every other statement is straight-line work. Function and
            // class definitions are opaque values here — their bodies get
            // their own CFGs when analyzed.
            _ => self.push(CfgStmt::Stmt(stmt)),
        }
    }

    fn build_loop(
        &mut self,
        target: Option<&'a ast::Expr>,
        test: Option<&'a ast::Expr>,
        iteration: LoopIteration,
        body: &'a [ast::Stmt],
        orelse: &'a [ast::Stmt],
    ) {
        let header = self.new_block();
        self.blocks[header.0].is_loop_header = true;
        self.blocks[header.0].iteration = iteration;
        // The outer repetition bound multiplied by this loop's trip count;
        // an unknown count (or overflow) opens the bound.
        let outer_repetitions = self.repetition_bound;
        let body_repetitions = match iteration {
            LoopIteration::Literal(count) => {
                outer_repetitions.and_then(|bound| bound.checked_mul(count))
            }
            LoopIteration::Unknown => None,
        };
        // The header itself runs once more than the body (the exit test).
        self.blocks[header.0].repetitions = match iteration {
            LoopIteration::Literal(count) => {
                outer_repetitions.and_then(|bound| bound.checked_mul(count.saturating_add(1)))
            }
            LoopIteration::Unknown => None,
        };
        self.seal(Terminator::Jump(header));

        let definite = matches!(iteration, LoopIteration::Literal(count) if count >= 2);
        self.cond_depth += 1;
        self.loop_depth += 1;
        self.repetition_bound = body_repetitions;
        if definite {
            self.definite_loops += 1;
        }
        let body_block = self.new_block();
        if definite {
            self.definite_loops -= 1;
        }
        self.repetition_bound = outer_repetitions;
        self.loop_depth -= 1;
        let orelse_block = if orelse.is_empty() {
            None
        } else {
            Some(self.new_block())
        };
        self.cond_depth -= 1;
        let after = self.new_block();

        self.blocks[header.0].terminator = Terminator::Branch {
            test,
            then_block: body_block,
            else_block: orelse_block.unwrap_or(after),
        };

        self.loop_stack.push(LoopFrame { header, after });
        self.cond_depth += 1;
        self.loop_depth += 1;
        self.repetition_bound = body_repetitions;
        if definite {
            self.definite_loops += 1;
        }
        self.resume_at(body_block);
        if let Some(target) = target {
            self.push(CfgStmt::LoopTarget(target));
        }
        self.walk(body);
        self.seal(Terminator::Jump(header));
        if definite {
            self.definite_loops -= 1;
        }
        self.repetition_bound = outer_repetitions;
        self.loop_depth -= 1;
        self.loop_stack.pop();

        if let Some(orelse_block) = orelse_block {
            self.resume_at(orelse_block);
            self.walk(orelse);
            self.seal(Terminator::Jump(after));
        }
        self.cond_depth -= 1;
        self.resume_at(after);
    }

    fn build_try(
        &mut self,
        body: &'a [ast::Stmt],
        handlers: &'a [ast::ExceptHandler],
        orelse: &'a [ast::Stmt],
        finalbody: &'a [ast::Stmt],
    ) {
        // Body and orelse run on the no-exception path; each handler is an
        // alternative entered (conservatively) from the pre-try state.
        let body_block;
        let handler_blocks: Vec<BlockId>;
        {
            self.cond_depth += 1;
            body_block = self.new_block();
            handler_blocks = handlers.iter().map(|_| self.new_block()).collect();
            self.cond_depth -= 1;
        }
        let join = self.new_block();

        let mut targets = vec![body_block];
        targets.extend(handler_blocks.iter().copied());
        self.seal(Terminator::Switch { targets });

        self.cond_depth += 1;
        self.resume_at(body_block);
        self.walk(body);
        self.walk(orelse);
        self.seal(Terminator::Jump(join));

        for (handler, block) in handlers.iter().zip(&handler_blocks) {
            let ast::ExceptHandler::ExceptHandler(handler) = handler;
            self.resume_at(*block);
            self.walk(&handler.body);
            self.seal(Terminator::Jump(join));
        }
        self.cond_depth -= 1;

        self.resume_at(join);
        // `finally` runs on the joined path. Early exits (return/raise in
        // the body) skip it here; see the module docs for the trade-off.
        self.walk(finalbody);
    }

    fn build_match(&mut self, inner: &'a ast::StmtMatch) {
        self.push(CfgStmt::Eval(&inner.subject));
        let case_blocks: Vec<BlockId>;
        {
            self.cond_depth += 1;
            case_blocks = inner.cases.iter().map(|_| self.new_block()).collect();
            self.cond_depth -= 1;
        }
        let join = self.new_block();

        let exhaustive = inner
            .cases
            .last()
            .is_some_and(|case| case.guard.is_none() && pattern_irrefutable(&case.pattern));
        let mut targets = case_blocks.clone();
        if !exhaustive {
            targets.push(join);
        }
        self.seal(Terminator::Switch { targets });

        self.cond_depth += 1;
        for (case, block) in inner.cases.iter().zip(&case_blocks) {
            self.resume_at(*block);
            self.push(CfgStmt::PatternBind(&case.pattern));
            if let Some(guard) = &case.guard {
                self.push(CfgStmt::Eval(guard));
            }
            self.walk(&case.body);
            self.seal(Terminator::Jump(join));
        }
        self.cond_depth -= 1;
        self.resume_at(join);
    }
}

/// Whether a pattern matches any subject (no test involved).
fn pattern_irrefutable(pattern: &ast::Pattern) -> bool {
    match pattern {
        // `case _:` and `case name:` (capture without inner pattern).
        ast::Pattern::MatchAs(inner) => inner.pattern.as_deref().is_none_or(pattern_irrefutable),
        ast::Pattern::MatchOr(inner) => inner.patterns.iter().any(pattern_irrefutable),
        _ => false,
    }
}

/// The literal trip count of a `range(...)` iterable, when every argument
/// is an integer literal (an empty span clamps to 0, exactly as `range`
/// iterates); [`LoopIteration::Unknown`] otherwise.
fn iteration_bound(iter: &ast::Expr) -> LoopIteration {
    let ast::Expr::Call(call) = iter else {
        return LoopIteration::Unknown;
    };
    let ast::Expr::Name(name) = call.func.as_ref() else {
        return LoopIteration::Unknown;
    };
    if name.id.as_str() != "range" || !call.keywords.is_empty() {
        return LoopIteration::Unknown;
    }
    let int_of = |expr: &ast::Expr| -> Option<i64> {
        let ast::Expr::Constant(constant) = expr else {
            return None;
        };
        let ast::Constant::Int(value) = &constant.value else {
            return None;
        };
        i64::try_from(value).ok()
    };
    let bound = (|| match call.args.as_slice() {
        [stop] => int_of(stop),
        [start, stop] => Some(int_of(stop)? - int_of(start)?),
        [start, stop, stride_expr] => {
            let stride = int_of(stride_expr)?;
            if stride <= 0 {
                return None;
            }
            let span = int_of(stop)? - int_of(start)?;
            Some(span.div_euclid(stride) + i64::from(span.rem_euclid(stride) > 0))
        }
        _ => None,
    })();
    match bound {
        Some(count) => LoopIteration::Literal(count.max(0)),
        None => LoopIteration::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::{Mode, ast, parse};

    fn body_of(source: &str) -> Vec<ast::Stmt> {
        let parsed = parse(source, Mode::Module, "test.py").expect("fixture parses");
        match parsed {
            ast::Mod::Module(module) => module.body,
            _ => panic!("expected module"),
        }
    }

    fn build(source: &str) -> (Vec<ast::Stmt>, usize) {
        let body = body_of(source);
        let block_count = ControlFlowGraph::build(&body).blocks.len();
        (body, block_count)
    }

    #[test]
    fn straight_line_is_one_block() {
        let body = body_of("x = 1\ny = 2\n");
        let cfg = ControlFlowGraph::build(&body);
        assert_eq!(cfg.blocks.len(), 1);
        assert_eq!(cfg.blocks[0].stmts.len(), 2);
        assert!(matches!(cfg.blocks[0].terminator, Terminator::End));
    }

    #[test]
    fn if_else_branches_and_joins() {
        let body = body_of("if cond:\n    a()\nelse:\n    b()\nc()\n");
        let cfg = ControlFlowGraph::build(&body);
        // entry, then, else, join.
        assert_eq!(cfg.blocks.len(), 4);
        let Terminator::Branch {
            test,
            then_block,
            else_block,
        } = &cfg.blocks[0].terminator
        else {
            panic!("entry must branch");
        };
        assert!(test.is_some());
        assert_eq!(cfg.blocks[then_block.0].cond_depth, 1);
        assert_eq!(cfg.blocks[else_block.0].cond_depth, 1);
        // Both arms jump to the join, which is unconditional again.
        let Terminator::Jump(join) = cfg.blocks[then_block.0].terminator else {
            panic!("then arm must jump to join");
        };
        assert!(matches!(
            cfg.blocks[else_block.0].terminator,
            Terminator::Jump(other) if other == join
        ));
        assert_eq!(cfg.blocks[join.0].cond_depth, 0);
        assert_eq!(cfg.blocks[join.0].stmts.len(), 1);
    }

    #[test]
    fn if_without_else_falls_through() {
        let body = body_of("if cond:\n    a()\nb()\n");
        let cfg = ControlFlowGraph::build(&body);
        let Terminator::Branch {
            then_block,
            else_block,
            ..
        } = &cfg.blocks[0].terminator
        else {
            panic!("entry must branch");
        };
        // The else edge goes straight to the join block.
        assert!(matches!(
            cfg.blocks[then_block.0].terminator,
            Terminator::Jump(join) if join == *else_block
        ));
    }

    #[test]
    fn while_loop_has_back_edge_and_header() {
        let body = body_of("while cond:\n    a()\nb()\n");
        let cfg = ControlFlowGraph::build(&body);
        let header = cfg
            .blocks
            .iter()
            .find(|block| block.is_loop_header)
            .expect("loop header exists");
        let Terminator::Branch {
            test, then_block, ..
        } = &header.terminator
        else {
            panic!("header must branch");
        };
        assert!(test.is_some());
        let body_block = &cfg.blocks[then_block.0];
        assert_eq!(body_block.loop_depth, 1);
        assert_eq!(body_block.cond_depth, 1);
        assert!(matches!(
            body_block.terminator,
            Terminator::Jump(back) if back == header.id
        ));
    }

    #[test]
    fn for_range_literal_marks_definite_iteration() {
        let body = body_of("for i in range(3):\n    a()\n");
        let cfg = ControlFlowGraph::build(&body);
        let header = cfg
            .blocks
            .iter()
            .find(|block| block.is_loop_header)
            .expect("loop header exists");
        assert_eq!(header.iteration, LoopIteration::Literal(3));
        let Terminator::Branch { then_block, .. } = &header.terminator else {
            panic!("header branches");
        };
        assert!(cfg.blocks[then_block.0].in_definite_loop);
        assert_eq!(cfg.blocks[then_block.0].repetitions, Some(3));

        let body = body_of("for i in items:\n    a()\n");
        let cfg = ControlFlowGraph::build(&body);
        let header = cfg
            .blocks
            .iter()
            .find(|block| block.is_loop_header)
            .expect("loop header exists");
        assert_eq!(header.iteration, LoopIteration::Unknown);
    }

    /// Per-block repetition bounds: literal `range` trip counts multiply
    /// through nested loops, an unknown iterable opens the bound (never 1),
    /// and code after a loop is back to exactly once per run.
    #[test]
    fn repetition_bounds_multiply_literal_loops_and_open_on_unknown() {
        let source = "\
before()
for i in range(3):
    for j in range(2, 8, 2):
        inner()
for x in items:
    unknown()
after()
";
        let body = body_of(source);
        let cfg = ControlFlowGraph::build(&body);
        let block_of = |call: &str| {
            cfg.blocks
                .iter()
                .find(|block| {
                    block.stmts.iter().any(|stmt| match stmt {
                        CfgStmt::Stmt(ast::Stmt::Expr(expr)) => {
                            matches!(&*expr.value, ast::Expr::Call(inner)
                                if matches!(&*inner.func, ast::Expr::Name(name)
                                    if name.id.as_str() == call))
                        }
                        _ => false,
                    })
                })
                .unwrap_or_else(|| panic!("no block holds {call}()"))
        };
        assert_eq!(block_of("before").repetitions, Some(1));
        // range(3) × range(2, 8, 2) = 3 × 3.
        assert_eq!(block_of("inner").repetitions, Some(9));
        assert_eq!(block_of("unknown").repetitions, None, "unknown trip count");
        assert_eq!(block_of("after").repetitions, Some(1));
    }

    #[test]
    fn break_skips_loop_else_and_continue_returns_to_header() {
        let body =
            body_of("while cond:\n    if x:\n        break\n    continue\nelse:\n    c()\nd()\n");
        let cfg = ControlFlowGraph::build(&body);
        let header = cfg
            .blocks
            .iter()
            .find(|block| block.is_loop_header)
            .expect("header")
            .id;
        let Terminator::Branch { else_block, .. } = cfg.blocks[header.0].terminator else {
            panic!("header branches");
        };
        // The header's else edge reaches the loop-else block (holding c()),
        // which jumps to the after block (holding d()).
        let Terminator::Jump(after) = cfg.blocks[else_block.0].terminator else {
            panic!("loop else jumps to after");
        };
        // break jumps directly to `after`, skipping the else block.
        let break_jumps: Vec<_> = cfg
            .blocks
            .iter()
            .filter(|block| {
                block.id != else_block
                    && matches!(block.terminator, Terminator::Jump(t) if t == after)
            })
            .collect();
        assert!(!break_jumps.is_empty(), "break must jump straight to after");
        // continue jumps back to the header from inside the body.
        assert!(
            cfg.blocks.iter().any(|block| block.loop_depth == 1
                && matches!(block.terminator, Terminator::Jump(t) if t == header)),
            "continue must jump to the header"
        );
    }

    #[test]
    fn return_and_raise_have_no_successors() {
        let body = body_of("if x:\n    return 1\nraise ValueError()\n");
        let cfg = ControlFlowGraph::build(&body);
        assert!(
            cfg.blocks
                .iter()
                .any(|block| matches!(block.terminator, Terminator::Return(Some(_))))
        );
        assert!(
            cfg.blocks
                .iter()
                .any(|block| matches!(block.terminator, Terminator::Raise))
        );
        for block in &cfg.blocks {
            if matches!(block.terminator, Terminator::Return(_) | Terminator::Raise) {
                assert!(cfg.successors(block.id).is_empty());
            }
        }
    }

    #[test]
    fn try_dispatches_to_body_and_handlers() {
        let body = body_of(
            "try:\n    a()\nexcept ValueError:\n    b()\nexcept KeyError:\n    c()\nfinally:\n    d()\n",
        );
        let cfg = ControlFlowGraph::build(&body);
        let Terminator::Switch { targets } = &cfg.blocks[0].terminator else {
            panic!("try entry must dispatch");
        };
        assert_eq!(targets.len(), 3, "body + two handlers");
        // All three paths converge on the join block that runs finally.
        let joins: std::collections::BTreeSet<_> = targets
            .iter()
            .map(|target| match cfg.blocks[target.0].terminator {
                Terminator::Jump(join) => join,
                _ => panic!("try arms jump to the join"),
            })
            .collect();
        assert_eq!(joins.len(), 1);
        let join = *joins.iter().next().unwrap();
        // finally body is straight-line work in the join block.
        assert_eq!(cfg.blocks[join.0].stmts.len(), 1);
        assert_eq!(cfg.blocks[join.0].cond_depth, 0);
    }

    #[test]
    fn match_switch_drops_fallthrough_when_exhaustive() {
        let (body, _) =
            build("match x:\n    case 1:\n        a()\n    case _:\n        b()\nc()\n");
        let cfg = ControlFlowGraph::build(&body);
        let Terminator::Switch { targets } = &cfg.blocks[0].terminator else {
            panic!("match must dispatch");
        };
        assert_eq!(targets.len(), 2, "wildcard case removes the skip edge");

        let (body, _) = build("match x:\n    case 1:\n        a()\nc()\n");
        let cfg = ControlFlowGraph::build(&body);
        let Terminator::Switch { targets } = &cfg.blocks[0].terminator else {
            panic!("match must dispatch");
        };
        assert_eq!(targets.len(), 2, "non-exhaustive match keeps the skip edge");
    }

    #[test]
    fn with_body_stays_inline() {
        let body = body_of("with open(p) as f:\n    a()\nb()\n");
        let cfg = ControlFlowGraph::build(&body);
        assert_eq!(cfg.blocks.len(), 1);
        assert!(matches!(cfg.blocks[0].stmts[0], CfgStmt::WithEnter(_)));
    }

    #[test]
    fn reverse_postorder_visits_branch_arms_before_join() {
        let body = body_of("if cond:\n    a()\nelse:\n    b()\nc()\n");
        let cfg = ControlFlowGraph::build(&body);
        let order = cfg.reverse_postorder();
        assert_eq!(order[0], cfg.entry);
        let position = |id: BlockId| order.iter().position(|other| *other == id).unwrap();
        let Terminator::Branch {
            then_block,
            else_block,
            ..
        } = cfg.blocks[0].terminator
        else {
            panic!("entry branches");
        };
        let Terminator::Jump(join) = cfg.blocks[then_block.0].terminator else {
            panic!("then jumps to join");
        };
        assert!(position(then_block) < position(join));
        assert!(position(else_block) < position(join));
        assert!(position(then_block) < position(else_block));
    }

    #[test]
    fn block_ids_are_deterministic() {
        let source = "if a:\n    b()\nfor i in range(3):\n    c()\n";
        let body_one = body_of(source);
        let body_two = body_of(source);
        let one = ControlFlowGraph::build(&body_one);
        let two = ControlFlowGraph::build(&body_two);
        assert_eq!(one.blocks.len(), two.blocks.len());
        for (a, b) in one.blocks.iter().zip(&two.blocks) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.loop_depth, b.loop_depth);
            assert_eq!(a.cond_depth, b.cond_depth);
            assert_eq!(a.is_loop_header, b.is_loop_header);
        }
    }
}
