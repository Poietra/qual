from manim import Scene, Square


class Branchy(Scene):
    def construct(self):
        sq = Square()
        if self.flag:
            self.add(sq)
        self.wait()


class Trying(Scene):
    def construct(self):
        sq = Square()
        try:
            self.add(sq)
        except ValueError:
            pass
        self.wait()
