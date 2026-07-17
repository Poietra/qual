from manim import Scene, Square as Sq
import manim as mn


class Demo(Scene):
    def construct(self):
        sq = Sq()
        self.play(mn.FadeIn(sq))
        sq.shift(mn.RIGHT)
