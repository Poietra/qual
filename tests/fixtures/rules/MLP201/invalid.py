from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda m: m.become(Text("hot")))
        label = always_redraw(lambda: MathTex(r"\alpha"))
        self.add(square, label)
        self.play(FadeIn(square), run_time=2)
