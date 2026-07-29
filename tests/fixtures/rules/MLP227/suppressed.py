from manim import Scene, Square


class Demo(Scene):
    def construct(self):
        self.always_update_mobjects = True
        square = Square()
        self.add(square)
        self.wait(3)  # qual: ignore[MLP227]
