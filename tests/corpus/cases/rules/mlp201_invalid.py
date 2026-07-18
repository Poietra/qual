from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Circle()
        square = Square()
        square.add_updater(lambda m: m.become(Text("hot")))
        label = always_redraw(lambda: MathTex(r"\alpha"))
        self.add(anchor, square, label)
        self.play(FadeIn(anchor), run_time=2)
