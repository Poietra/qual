import manim as mn


class Demo(mn.Scene):
    def construct(self):
        square = mn.Square()
        dot = mn.Dot()
        self.play(mn.FadeIn(square), mn.FadeIn(dot), lag_ratio=0.5)
