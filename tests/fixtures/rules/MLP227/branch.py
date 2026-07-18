from manim import Scene, Square


class Demo(Scene):
    def construct(self):
        self.always_update_mobjects = bool(1)
        square = Square()
        self.add(square)
        self.wait(3)
