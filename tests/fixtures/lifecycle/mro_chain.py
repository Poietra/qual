from manim import Circle, Scene, Square


class Base(Scene):
    def setup(self):
        self.background = Square()
        self.add(self.background)


class Child(Base):
    def setup(self):
        super().setup()
        self.dot = Circle()
        self.add(self.dot)

    def construct(self):
        self.remove(self.background)


class NoSuper(Base):
    def setup(self):
        self.dot = Circle()

    def construct(self):
        self.add(self.dot)
