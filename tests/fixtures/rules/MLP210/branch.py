from manim import FadeIn, Scene, Square


class Demo(Scene):
    def construct(self, count=20):
        square = Square()
        for _ in range(count):
            self.play(FadeIn(square), run_time=0.5)
        for index in range(20):
            self.play(FadeIn(square), run_time=0.5)
            if index > 5:
                break
