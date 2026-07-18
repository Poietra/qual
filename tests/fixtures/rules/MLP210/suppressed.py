from manim import FadeIn, Scene, Square


class Demo(Scene):
    def construct(self):
        square = Square()
        for _ in range(10):
            self.play(FadeIn(square), run_time=0.5)  # manim-lint: ignore[MLP210]
