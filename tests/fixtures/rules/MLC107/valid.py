from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        sq.generate_target()
        self.play(MoveToTarget(sq))
        circle = Circle()
        self.play(circle.animate.shift(LEFT))
        self.play(MoveToTarget(circle))


class LoopGenerated(Scene):
    # Regression: `generate_target()` inside a loop, consumed after it.
    # Abstractly the loop may run zero times, so presence is Maybe, and
    # MLC107 (all-paths-absent) must stay silent. A pre-fixpoint fact
    # emission bug used to report this as absent-on-all-paths.
    def construct(self):
        c = Circle()
        for i in range(3):
            c.generate_target()
        self.play(MoveToTarget(c))
        d = Dot()
        n = 3
        while n > 0:
            d.generate_target()
            n -= 1
        self.play(MoveToTarget(d))
