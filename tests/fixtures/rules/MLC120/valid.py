from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        sq.save_state()
        self.play(sq.animate.shift(RIGHT))
        self.play(Restore(sq))
        sq.restore()


class LoopSaved(Scene):
    # Regression: `save_state()` inside a loop, restored after it.
    # Abstractly the loop may run zero times, so presence is Maybe, and
    # MLC120 (all-paths-absent) must stay silent. A pre-fixpoint fact
    # emission bug used to report this as absent-on-all-paths.
    def construct(self):
        c = Circle()
        for i in range(2):
            c.save_state()
        self.play(Restore(c))
