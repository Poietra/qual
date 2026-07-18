from manim import FadeIn, Scene, Square


class Demo(Scene):
    def construct(self):
        square = Square()
        for _ in range(3):
            self.play(FadeIn(square), run_time=0.5)
        for index in range(20):
            if index % 2 == 0:
                self.play(FadeIn(square), run_time=0.5)
