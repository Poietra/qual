from manim import Scene, Square


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(lambda m, dt: None)  # manim-lint: ignore[MLP215]
        self.wait(3)
