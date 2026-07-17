import manim as mn


class StyleScene(mn.Scene):
    def construct(self):
        square = mn.Square()
        self.play(mn.FadeIn(square), run_time=2)
