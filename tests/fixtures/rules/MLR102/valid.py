from manim import *


class Good(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        # A chained method call is the normal use.
        self.play(square.animate.shift(RIGHT))
        # A builder that is never played does nothing to warn about here.
        unplayed = square.animate
        # The target is not displayed yet: the play auto-adds it, which is
        # a visible effect, not a no-op.
        fresh = Dot()
        self.play(fresh.animate)
        # The live object changed after the builder snapshot: playing the
        # stale target snaps it back, which is visible (MLC117 territory).
        stale = Square()
        self.add(stale)
        builder = stale.animate
        stale.shift(LEFT)
        self.play(builder)
